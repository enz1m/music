//! Route controls for the bounded playlist parser and serializer through the real
//! [`crate::playlist_io`] `parse` and `serialize` entry points.

use std::path::{Path, PathBuf};

use crate::playlist_io::{
    MAX_DOCUMENT_BYTES, MAX_ENTRIES, ParseError, PlaylistFormat, parse, serialize,
};

fn base() -> &'static Path {
    Path::new("/music/lists")
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn m3u_relative_parent_duplicate_and_refusal() {
    let doc = b"#EXTM3U\n\
        #EXTINF:123,Some Song\n\
        song.mp3\n\
        ../other/two.mp3\n\
        song.mp3\n\
        /abs/three.mp3\n\
        http://stream.example/live\n";
    let report = parse(doc, PlaylistFormat::M3u8, base()).expect("valid M3U document");
    assert_eq!(
        report.paths,
        vec![
            p("/music/lists/song.mp3"),
            p("/music/other/two.mp3"),
            p("/abs/three.mp3"),
        ],
        "relative joins the parent, `..` resolves lexically, absolute stands"
    );
    assert_eq!(
        report.duplicate, 1,
        "the repeated song.mp3 is one duplicate"
    );
    assert_eq!(report.refused, 1, "the http URI is one refused row");
}

#[test]
fn pls_numeric_ordering() {
    let doc = b"[playlist]\n\
        File2=b.mp3\n\
        File10=c.mp3\n\
        File1=a.mp3\n\
        Title1=ignored\n\
        Length1=-1\n\
        NumberOfEntries=3\n\
        Version=2\n";
    let report = parse(doc, PlaylistFormat::Pls, base()).expect("valid PLS document");
    assert_eq!(
        report.paths,
        vec![
            p("/music/lists/a.mp3"),
            p("/music/lists/b.mp3"),
            p("/music/lists/c.mp3"),
        ],
        "numeric ordering by index: 1, 2, 10"
    );
    assert_eq!(report.refused, 0);
    assert_eq!(report.duplicate, 0);
}

#[test]
fn entry_count_bound_exact_and_plus_one() {
    let mut exact = String::from("#EXTM3U\n");
    for i in 0..MAX_ENTRIES {
        exact.push_str(&format!("/m/{i}.mp3\n"));
    }
    let report = parse(exact.as_bytes(), PlaylistFormat::M3u, base()).expect("exact is admitted");
    assert_eq!(report.paths.len(), MAX_ENTRIES);

    let mut over = exact;
    over.push_str(&format!("/m/{MAX_ENTRIES}.mp3\n"));
    let err = parse(over.as_bytes(), PlaylistFormat::M3u, base()).expect_err("one over refuses");
    assert!(matches!(err, ParseError::TooManyEntries { .. }));
}

#[test]
fn document_byte_bound_plus_one() {
    let bytes = vec![b'a'; MAX_DOCUMENT_BYTES + 1];
    let err =
        parse(&bytes, PlaylistFormat::M3u, base()).expect_err("over-large document is refused");
    assert!(matches!(err, ParseError::DocumentTooLarge { .. }));
}

#[test]
fn malformed_pls_is_atomic() {
    assert!(matches!(
        parse(b"File1=a.mp3\n", PlaylistFormat::Pls, base()),
        Err(ParseError::MissingPlaylistSection)
    ));
    assert!(matches!(
        parse(b"[playlist]\ngarbage line\n", PlaylistFormat::Pls, base()),
        Err(ParseError::Malformed { line: 2 })
    ));
    assert!(matches!(
        parse(
            b"[playlist]\nFile1=a\nFile1=b\n",
            PlaylistFormat::Pls,
            base()
        ),
        Err(ParseError::Malformed { .. })
    ));
    assert!(matches!(
        parse(
            b"[playlist]\nNumberOfEntries=1\nNumberOfEntries=2\n",
            PlaylistFormat::Pls,
            base()
        ),
        Err(ParseError::Malformed { .. })
    ));
    assert!(matches!(
        parse(
            b"[playlist]\nFile1=a\n[extra]\n",
            PlaylistFormat::Pls,
            base()
        ),
        Err(ParseError::Malformed { .. })
    ));
}

#[test]
fn path_refusal_policy() {
    let doc = "#EXTM3U\n\
        \u{0001}control.mp3\n\
        smb://server/share/x.mp3\n\
        file://user:pass@host/creds.mp3\n\
        file:///music/ok%20one.mp3\n"
        .to_string();
    let report = parse(doc.as_bytes(), PlaylistFormat::M3u8, base()).expect("document parses");
    assert_eq!(
        report.paths,
        vec![p("/music/ok one.mp3")],
        "only the valid file URI is admitted, percent-decoded"
    );
    assert_eq!(report.refused, 3, "control, non-file scheme, credentials");
}

#[test]
fn round_trip_through_opposite_parser() {
    let dest = Path::new("/music/lists");
    let paths = vec![p("/music/lists/inside.mp3"), p("/elsewhere/outside.mp3")];

    for format in [PlaylistFormat::M3u8, PlaylistFormat::Pls] {
        let bytes = serialize(&paths, format, dest).expect("serialize succeeds");
        let text = String::from_utf8(bytes.clone()).expect("output is UTF-8");
        assert!(
            text.contains("inside.mp3") && !text.contains("/music/lists/inside.mp3"),
            "a descendant is written relative for {format:?}: {text}"
        );
        assert!(
            text.contains("/elsewhere/outside.mp3"),
            "an outside path is written absolute for {format:?}: {text}"
        );
        let report = parse(&bytes, format, dest).expect("re-parse succeeds");
        assert_eq!(
            report.paths, paths,
            "round-trip preserves paths for {format:?}"
        );
        assert_eq!(report.refused, 0);
        assert_eq!(report.duplicate, 0);
    }
}
