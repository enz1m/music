//! Bounded playlist document parsing and serialization for M3U / M3U8 and PLS.
//!
//! Pure and `std`-only: callers read and write bytes elsewhere, and the reducer
//! acts only on a fully settled [`ParseReport`], so a malformed document never
//! partially mutates a playlist.
//!
//! Every bound is checked before the value it guards is retained: document
//! bytes, line count, line bytes, path bytes, entry count and aggregate path
//! bytes (the `MAX_*` constants). Size and structural faults refuse the whole
//! document ([`ParseError`]); an unusable row is counted in
//! [`ParseReport::refused`].
//!
//! All three formats are read as UTF-8 with an optional BOM; no locale codepage
//! is guessed, since persisted paths are UTF-8.
//!
//! [`resolve_entry`] joins relative entries to the playlist's parent and
//! normalizes lexically without canonicalizing, so missing files keep their
//! names. `file:` URIs are accepted only with an empty or `localhost` authority
//! and no credentials; other schemes are refused. Duplicates keep the first
//! occurrence.

use std::path::{Component, Path, PathBuf};

use crate::domain::{MAX_PERSISTED_PATH_BYTES, MAX_TRACKS_PER_PLAYLIST};

/// Maximum exact encoded byte length of a whole playlist document, checked
/// before any line is decoded.
pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of physical lines, bounding a comment or blank-line flood.
pub const MAX_LINES: usize = 400_000;

/// Maximum bytes in one physical line: [`MAX_PATH_BYTES`] plus room for a
/// `FileN=` prefix and CR.
pub const MAX_LINE_BYTES: usize = MAX_PATH_BYTES + 64;

/// Maximum encoded byte length of any single resolved path, matching the
/// persisted-path limit (`PATH_MAX`, 4096) so admitted paths can be reloaded.
pub const MAX_PATH_BYTES: usize = MAX_PERSISTED_PATH_BYTES;

/// Maximum number of admitted entries, matching the per-playlist track
/// ceiling. A document offering more is refused whole.
pub const MAX_ENTRIES: usize = MAX_TRACKS_PER_PLAYLIST;

/// Maximum aggregate encoded bytes of every retained path, which a dense file
/// of short paths could otherwise reach under the document ceiling.
pub const MAX_AGGREGATE_PATH_BYTES: usize = 32 * 1024 * 1024;

/// The three playlist document formats this module reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistFormat {
    /// Extended M3U, read/written as UTF-8 (see the module encoding policy).
    M3u,
    /// M3U8: UTF-8 by definition.
    M3u8,
    /// PLS: an INI-shaped `[playlist]` document.
    Pls,
}

impl PlaylistFormat {
    /// The format named by a path's extension, case-insensitively, or `None`
    /// when the extension is not one this module owns.
    #[must_use]
    pub fn from_extension(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "m3u" => Some(Self::M3u),
            "m3u8" => Some(Self::M3u8),
            "pls" => Some(Self::Pls),
            _ => None,
        }
    }

    /// Whether this format's line grammar is the M3U family (as opposed to PLS).
    fn is_m3u(self) -> bool {
        matches!(self, Self::M3u | Self::M3u8)
    }
}

/// A document-level parse fault. Every variant refuses the whole document
/// before any live state is touched; per-*row* refusals are counted in
/// [`ParseReport::refused`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The encoded document exceeds [`MAX_DOCUMENT_BYTES`].
    DocumentTooLarge { bytes: usize, limit: usize },
    /// The document has more than [`MAX_LINES`] physical lines.
    TooManyLines { limit: usize },
    /// A physical line exceeds [`MAX_LINE_BYTES`].
    LineTooLong { line: usize, limit: usize },
    /// The document offers more than [`MAX_ENTRIES`] admissible entries.
    TooManyEntries { limit: usize },
    /// Retaining the admitted paths would exceed [`MAX_AGGREGATE_PATH_BYTES`].
    AggregatePathBytesExceeded { limit: usize },
    /// The document is not valid UTF-8 (see the module encoding policy).
    NotUtf8,
    /// A PLS document with no `[playlist]` section header.
    MissingPlaylistSection,
    /// Structurally malformed at 1-based physical `line` (a non-comment line
    /// with no `=`, an unexpected section header, a repeated singleton key, a
    /// repeated or non-positive `FileN` index).
    Malformed { line: usize },
}

impl ParseError {
    /// A bounded, user-safe one-line summary. Never includes document bytes.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::DocumentTooLarge { .. } => "That playlist file is too large to import.".into(),
            Self::TooManyLines { .. } => "That playlist file has too many lines to import.".into(),
            Self::LineTooLong { .. } => "That playlist file has an over-long line.".into(),
            Self::TooManyEntries { .. } => "That playlist file has too many entries.".into(),
            Self::AggregatePathBytesExceeded { .. } => {
                "That playlist file lists too many paths to import.".into()
            }
            Self::NotUtf8 => "That playlist file is not valid UTF-8.".into(),
            Self::MissingPlaylistSection => "That PLS file has no [playlist] section.".into(),
            Self::Malformed { .. } => "That playlist file is malformed.".into(),
        }
    }
}

/// The settled outcome of parsing one document: ordered, deduplicated,
/// resolved absolute paths plus refusal and duplicate counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseReport {
    /// The format the document was read as.
    #[allow(dead_code)]
    pub format: PlaylistFormat,
    /// Resolved absolute paths in document order (PLS: numeric `FileN` order),
    /// deduplicated by durable normalized path.
    pub paths: Vec<PathBuf>,
    /// Rows refused during parse (control byte, non-file scheme, credentials,
    /// overlong, non-UTF-8, empty-after-decode).
    pub refused: usize,
    /// Rows dropped as duplicates of an already-admitted path.
    pub duplicate: usize,
}

/// Why one raw entry was refused as a row (never a document fault).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRefusal {
    /// Empty after trimming / URI decoding.
    Empty,
    /// Contained a NUL, other C0 control, or DEL.
    Control,
    /// A URI whose scheme is not `file`.
    NonFileScheme,
    /// A `file:` URI carrying `user[:pass]@` credentials.
    Credentials,
    /// A `file:` URI with a non-empty, non-`localhost` authority.
    NonLocalUri,
    /// A malformed percent-escape in a `file:` URI.
    BadEscape,
    /// The resolved path exceeds [`MAX_PATH_BYTES`].
    TooLong,
    /// The decoded `file:` URI path is not valid UTF-8.
    NotUtf8,
    /// The resolved path is empty or names only the filesystem root.
    Degenerate,
}

/// Parse `bytes` as `format`, resolving relative entries against `base_dir`
/// (the selected playlist file's parent directory).
///
/// Returns a whole [`ParseReport`] or a document-level [`ParseError`]; a
/// single unusable row is counted, not fatal.
pub fn parse(
    bytes: &[u8],
    format: PlaylistFormat,
    base_dir: &Path,
) -> Result<ParseReport, ParseError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(ParseError::DocumentTooLarge {
            bytes: bytes.len(),
            limit: MAX_DOCUMENT_BYTES,
        });
    }
    // Optional leading UTF-8 BOM, then the whole document must be valid UTF-8.
    let body = strip_bom(bytes);
    let text = std::str::from_utf8(body).map_err(|_| ParseError::NotUtf8)?;

    if format.is_m3u() {
        parse_m3u(text, format, base_dir)
    } else {
        parse_pls(text, base_dir)
    }
}

/// Strip a single leading UTF-8 BOM (`EF BB BF`) if present.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Enumerate the physical lines of `text`, enforcing the line-count and
/// per-line-byte ceilings. Each yielded line has its trailing `\r` removed.
fn physical_lines(text: &str) -> Result<Vec<&str>, ParseError> {
    let mut lines = Vec::new();
    for (idx, raw) in text.split('\n').enumerate() {
        if lines.len() >= MAX_LINES {
            return Err(ParseError::TooManyLines { limit: MAX_LINES });
        }
        if raw.len() > MAX_LINE_BYTES {
            return Err(ParseError::LineTooLong {
                line: idx + 1,
                limit: MAX_LINE_BYTES,
            });
        }
        lines.push(raw.strip_suffix('\r').unwrap_or(raw));
    }
    Ok(lines)
}

/// A per-document accumulator that admits deduplicated paths under the entry
/// count and aggregate-byte ceilings.
struct Admission {
    paths: Vec<PathBuf>,
    seen: std::collections::HashSet<PathBuf>,
    aggregate_bytes: usize,
    refused: usize,
    duplicate: usize,
}

impl Admission {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            seen: std::collections::HashSet::new(),
            aggregate_bytes: 0,
            refused: 0,
            duplicate: 0,
        }
    }

    /// Admit a resolved path, counting a duplicate, or return a document-level
    /// error when a ceiling is crossed.
    fn admit(&mut self, path: PathBuf) -> Result<(), ParseError> {
        if self.seen.contains(&path) {
            self.duplicate += 1;
            return Ok(());
        }
        if self.paths.len() >= MAX_ENTRIES {
            return Err(ParseError::TooManyEntries { limit: MAX_ENTRIES });
        }
        let bytes = path.as_os_str().len();
        let next = self.aggregate_bytes.saturating_add(bytes);
        if next > MAX_AGGREGATE_PATH_BYTES {
            return Err(ParseError::AggregatePathBytesExceeded {
                limit: MAX_AGGREGATE_PATH_BYTES,
            });
        }
        self.aggregate_bytes = next;
        self.seen.insert(path.clone());
        self.paths.push(path);
        Ok(())
    }

    /// Route a row-resolution outcome: admit the path or count the refusal.
    fn resolved(&mut self, outcome: Result<PathBuf, RowRefusal>) -> Result<(), ParseError> {
        match outcome {
            Ok(path) => self.admit(path),
            Err(_) => {
                self.refused += 1;
                Ok(())
            }
        }
    }

    fn into_report(self, format: PlaylistFormat) -> ParseReport {
        ParseReport {
            format,
            paths: self.paths,
            refused: self.refused,
            duplicate: self.duplicate,
        }
    }
}

/// Parse an M3U / M3U8 document: one path per non-comment, non-blank line;
/// `#` lines (including `#EXTM3U` and `#EXTINF`) are bounded metadata and
/// ignored.
fn parse_m3u(
    text: &str,
    format: PlaylistFormat,
    base_dir: &Path,
) -> Result<ParseReport, ParseError> {
    let lines = physical_lines(text)?;
    let mut admission = Admission::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        admission.resolved(resolve_entry(trimmed, base_dir))?;
    }
    Ok(admission.into_report(format))
}

/// Parse a PLS document: a required `[playlist]` section of `FileN=path` keys,
/// with `TitleN`/`LengthN` ignored, `NumberOfEntries`/`Version` as bounded
/// singletons, and deterministic numeric ordering by `N`.
fn parse_pls(text: &str, base_dir: &Path) -> Result<ParseReport, ParseError> {
    let lines = physical_lines(text)?;
    let mut in_playlist = false;
    let mut saw_playlist = false;
    let mut number_of_entries: Option<usize> = None;
    let mut version: Option<String> = None;
    // (index, raw path) collected in-order, sorted numerically before admission.
    let mut files: Vec<(usize, String)> = Vec::new();
    let mut file_bytes = 0usize;
    let mut file_indices = std::collections::HashSet::new();

    for (zero_based, raw) in lines.iter().enumerate() {
        let line_no = zero_based + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Section header.
        if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if inner.eq_ignore_ascii_case("playlist") {
                in_playlist = true;
                saw_playlist = true;
                continue;
            }
            // PLS defines exactly one section; any other is malformed structure.
            return Err(ParseError::Malformed { line: line_no });
        }
        // Content before the `[playlist]` header reports the section as missing.
        if !in_playlist {
            return Err(ParseError::MissingPlaylistSection);
        }
        let Some(eq) = trimmed.find('=') else {
            return Err(ParseError::Malformed { line: line_no });
        };
        let key = trimmed[..eq].trim();
        let value = trimmed[eq + 1..].trim();
        let key_lower = key.to_ascii_lowercase();

        if key_lower == "numberofentries" {
            if number_of_entries.is_some() {
                return Err(ParseError::Malformed { line: line_no });
            }
            number_of_entries = Some(
                value
                    .parse()
                    .map_err(|_| ParseError::Malformed { line: line_no })?,
            );
            continue;
        }
        if key_lower == "version" {
            if version.is_some() {
                return Err(ParseError::Malformed { line: line_no });
            }
            // PLS is version 2; a declared other version is refused structure.
            if value != "2" {
                return Err(ParseError::Malformed { line: line_no });
            }
            version = Some(value.to_string());
            continue;
        }
        if let Some(n) = suffix_index(&key_lower, "file") {
            if n == 0 || !file_indices.insert(n) {
                // Non-positive or repeated index is malformed structure.
                return Err(ParseError::Malformed { line: line_no });
            }
            if files.len() >= MAX_ENTRIES {
                return Err(ParseError::TooManyEntries { limit: MAX_ENTRIES });
            }
            file_bytes = file_bytes.checked_add(value.len()).ok_or(
                ParseError::AggregatePathBytesExceeded {
                    limit: MAX_AGGREGATE_PATH_BYTES,
                },
            )?;
            if file_bytes > MAX_AGGREGATE_PATH_BYTES {
                return Err(ParseError::AggregatePathBytesExceeded {
                    limit: MAX_AGGREGATE_PATH_BYTES,
                });
            }
            files.push((n, value.to_string()));
            continue;
        }
        // Title/Length carry a positive index and are ignored after admission.
        if suffix_index(&key_lower, "title").is_some()
            || suffix_index(&key_lower, "length").is_some()
        {
            continue;
        }
        // Any other key is ignored (a tolerant extension point), not fatal.
    }

    if !saw_playlist {
        return Err(ParseError::MissingPlaylistSection);
    }

    // Deterministic numeric ordering by index, then admit in that order.
    files.sort_by_key(|(n, _)| *n);
    let mut admission = Admission::new();
    for (_, raw) in files {
        admission.resolved(resolve_entry(&raw, base_dir))?;
    }
    // `NumberOfEntries` is advisory; a mismatch with the admitted count is
    // tolerated.
    let _ = (number_of_entries, version);
    Ok(admission.into_report(PlaylistFormat::Pls))
}

/// The trailing decimal index of `key` after `prefix` (case already folded),
/// e.g. `("file12", "file") -> Some(12)`; `None` when `key` is not `prefix`
/// followed by ASCII digits.
fn suffix_index(key: &str, prefix: &str) -> Option<usize> {
    let rest = key.strip_prefix(prefix)?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Resolve one raw document entry to a durable absolute path, or the reason it
/// is refused.
fn resolve_entry(raw: &str, base_dir: &Path) -> Result<PathBuf, RowRefusal> {
    let entry = raw.trim();
    if entry.is_empty() {
        return Err(RowRefusal::Empty);
    }
    if has_control(entry) {
        return Err(RowRefusal::Control);
    }

    let candidate = if is_file_uri(entry) {
        parse_file_uri(entry)?
    } else if let Some(scheme) = uri_scheme(entry) {
        // A non-file URI scheme (http, https, smb, mms, …) is not a local file.
        let _ = scheme;
        return Err(RowRefusal::NonFileScheme);
    } else {
        PathBuf::from(entry)
    };

    // Join relative entries to the playlist's parent; absolute entries stand.
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    };
    let normalized = lexical_normalize(&joined);

    // A control byte can only arrive through a percent-escape in a file: URI;
    // re-check the decoded path.
    if normalized.as_os_str().to_str().is_some_and(has_control) {
        return Err(RowRefusal::Control);
    }
    if normalized.as_os_str().len() > MAX_PATH_BYTES {
        return Err(RowRefusal::TooLong);
    }
    if is_degenerate(&normalized) {
        return Err(RowRefusal::Degenerate);
    }
    Ok(normalized)
}

/// Whether `s` carries a NUL, another C0 control, or DEL.
fn has_control(s: &str) -> bool {
    s.bytes().any(|b| b < 0x20 || b == 0x7F)
}

/// Whether `entry` begins with a `file:` scheme (case-insensitive). Compared
/// over bytes so a leading multi-byte scalar cannot force a non-char-boundary
/// slice panic.
fn is_file_uri(entry: &str) -> bool {
    let b = entry.as_bytes();
    b.len() >= 5 && b[..5].eq_ignore_ascii_case(b"file:")
}

/// The URI scheme of `entry` if it is shaped `scheme:` where `scheme` is a
/// valid RFC-3986 scheme (ALPHA then ALPHA/DIGIT/`+`/`-`/`.`). Refusing every
/// such non-file prefix avoids treating opaque remote URIs as local paths.
fn uri_scheme(entry: &str) -> Option<&str> {
    let colon = entry.find(':')?;
    let scheme = &entry[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some(scheme)
}

/// Parse a `file:` URI into an absolute path, enforcing authority and escape
/// policy.
fn parse_file_uri(entry: &str) -> Result<PathBuf, RowRefusal> {
    let rest = &entry[5..]; // after "file:"
    let (authority, path_part) = if let Some(after) = rest.strip_prefix("//") {
        match after.find('/') {
            Some(i) => (&after[..i], &after[i..]),
            // `file://host` with no path names no file.
            None => return Err(RowRefusal::Empty),
        }
    } else {
        // `file:/path` (no authority) is accepted; the path is `rest`.
        ("", rest)
    };
    if authority.contains('@') {
        return Err(RowRefusal::Credentials);
    }
    if !(authority.is_empty() || authority.eq_ignore_ascii_case("localhost")) {
        return Err(RowRefusal::NonLocalUri);
    }
    if path_part.is_empty() {
        return Err(RowRefusal::Empty);
    }
    let decoded = percent_decode(path_part)?;
    let text = String::from_utf8(decoded).map_err(|_| RowRefusal::NotUtf8)?;
    Ok(PathBuf::from(text))
}

/// Percent-decode a URI path, refusing a malformed escape. Bytes are returned
/// so the caller can enforce the UTF-8 policy explicitly.
fn percent_decode(s: &str) -> Result<Vec<u8>, RowRefusal> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(RowRefusal::BadEscape);
                }
                let hi = hex_val(bytes[i + 1]).ok_or(RowRefusal::BadEscape)?;
                let lo = hex_val(bytes[i + 2]).ok_or(RowRefusal::BadEscape)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// The value of one ASCII hex digit, or `None`.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Lexically normalize a path, resolving `.` and `..` without touching the
/// filesystem so a missing entry keeps its durable name. A leading `..` beyond
/// an absolute root is dropped (it cannot escape `/`); a relative result keeps
/// leading `..` components.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut is_absolute = false;
    let mut stack: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) => {} // no Windows prefixes on Linux
            Component::RootDir => {
                is_absolute = true;
                stack.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Decide before mutating so the `stack.last()` borrow ends before
                // `pop`/`push`.
                if matches!(stack.last(), Some(Component::Normal(_))) {
                    stack.pop();
                } else if !is_absolute {
                    // A leading `..` in a relative path is retained; at an
                    // absolute root `..` is a no-op that cannot escape `/`.
                    stack.push(Component::ParentDir);
                }
            }
            normal @ Component::Normal(_) => stack.push(normal),
        }
    }
    let mut out = PathBuf::new();
    if is_absolute {
        out.push(Component::RootDir.as_os_str());
    }
    for comp in stack {
        out.push(comp.as_os_str());
    }
    // A normalized absolute path that collapsed to nothing is the root itself.
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Whether a resolved path is unusable as a durable entry: empty, or naming
/// only the filesystem root.
fn is_degenerate(path: &Path) -> bool {
    path.as_os_str().is_empty() || path == Path::new("/") || path == Path::new(".")
}

/// A serialization fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerializeError {
    /// A path exceeds [`MAX_PATH_BYTES`] once rendered.
    PathTooLong,
    /// A path is not representable as UTF-8, so it could not be reloaded.
    NotUtf8,
    /// More than [`MAX_ENTRIES`] paths were offered.
    TooManyEntries,
}

/// Serialize `paths` as `format`, using paths **relative** to `dest_dir` only
/// when they remain under it, and absolute paths otherwise.
///
/// Order is preserved exactly. Line endings are `\n` and no BOM is written.
pub fn serialize(
    paths: &[PathBuf],
    format: PlaylistFormat,
    dest_dir: &Path,
) -> Result<Vec<u8>, SerializeError> {
    if paths.len() > MAX_ENTRIES {
        return Err(SerializeError::TooManyEntries);
    }
    // Normalize the destination parent once so descendant detection is a
    // component comparison, not a textual prefix match.
    let dest = lexical_normalize(dest_dir);
    let mut rendered = Vec::with_capacity(paths.len());
    for path in paths {
        let text = render_path(path, &dest)?;
        rendered.push(text);
    }
    let mut out = String::new();
    match format {
        PlaylistFormat::M3u | PlaylistFormat::M3u8 => {
            out.push_str("#EXTM3U\n");
            for line in &rendered {
                out.push_str(line);
                out.push('\n');
            }
        }
        PlaylistFormat::Pls => {
            out.push_str("[playlist]\n");
            for (i, line) in rendered.iter().enumerate() {
                out.push_str(&format!("File{}={}\n", i + 1, line));
            }
            out.push_str(&format!("NumberOfEntries={}\n", rendered.len()));
            out.push_str("Version=2\n");
        }
    }
    Ok(out.into_bytes())
}

/// Render one durable path for serialization: relative to `dest_dir` when it is
/// a strict descendant, else absolute. Enforces the UTF-8 and byte-length
/// policy so the output can always be reloaded.
fn render_path(path: &Path, dest_dir: &Path) -> Result<String, SerializeError> {
    let normalized = lexical_normalize(path);
    let rel = normalized
        .strip_prefix(dest_dir)
        .ok()
        .filter(|r| !r.as_os_str().is_empty());
    let chosen: &Path = rel.unwrap_or(&normalized);
    let text = chosen.to_str().ok_or(SerializeError::NotUtf8)?;
    if text.len() > MAX_PATH_BYTES {
        return Err(SerializeError::PathTooLong);
    }
    Ok(text.to_string())
}
