// Artist name splitting

/// Split a combined artist string into individual artist names.
///
/// Handles: `,` `;` `&` `+` `/` list separators,
/// `feat.`/`feat`/`featuring`/`ft.`/`ft` (case-insensitive),
/// ` x ` and ` vs ` / ` vs. ` (whitespace-bounded).
///
/// "Azahriah, DESH" → ["Azahriah", "DESH"]
/// "Solo Artist feat. Guest" → ["Solo Artist", "Guest"]
pub fn split_artist_names(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }

    const SEP: char = '\u{1F}';
    let lower = raw.to_lowercase();
    let mut marks: Vec<(usize, usize)> = Vec::new();

    const TEXT_SEPS: &[&str] = &[
        " featuring ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " vs. ",
        " vs ",
        " x ",
    ];

    for pat in TEXT_SEPS {
        let mut start = 0usize;
        while let Some(idx) = lower[start..].find(pat) {
            let abs = start + idx;
            marks.push((abs, pat.len()));
            start = abs + pat.len();
        }
    }

    for (i, ch) in raw.char_indices() {
        if matches!(ch, ',' | ';' | '&' | '+' | '/') {
            marks.push((i, ch.len_utf8()));
        }
    }

    marks.sort_by_key(|(s, _)| *s);
    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    for (start, len) in marks {
        if start < cursor {
            continue;
        }
        out.push_str(&raw[cursor..start]);
        out.push(SEP);
        cursor = start + len;
    }
    out.push_str(&raw[cursor..]);

    out.split(SEP)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
