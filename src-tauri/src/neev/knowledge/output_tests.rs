//! Task 8: output validation (pure).

use super::output::*;
use super::types::*;
use crate::neev::agents::generated::GeneratedFile;
use std::collections::{BTreeMap, BTreeSet};

fn file(name: &str, body: &str) -> GeneratedFile {
    GeneratedFile { name: name.into(), bytes: body.as_bytes().to_vec(), sha256: String::new() }
}
fn labels() -> BTreeSet<String> {
    ["u01", "u02"].iter().map(|s| s.to_string()).collect()
}
fn keys() -> BTreeSet<String> {
    ["k-0123456789ab"].iter().map(|s| s.to_string()).collect()
}

#[test]
fn parses_pages_citations_and_skips_and_ignores_other_files() {
    let parsed = parse_outputs(
        &[
            file("wiki--onboarding.md", "# Onboarding\n\nUse the join code.[^u01] Kept.[^k-0123456789ab]\n"),
            file("knowledge-skipped.json", r#"[{"unit":"u02","reason":"chit-chat"},{"unit":"u07","reason":"x"}]"#),
            file("notes.txt", "[^u01]"),
            file("wiki--Bad_Slug.md", "# A"),
        ],
        &labels(),
        &keys(),
    )
    .unwrap();
    assert_eq!(parsed.pages.len(), 1);
    assert_eq!((parsed.pages[0].slug.as_str(), parsed.pages[0].title.as_str()), ("onboarding", "Onboarding"));
    assert!(parsed.pages[0].cited.contains("u01") && parsed.pages[0].cited.contains("k-0123456789ab"));
    assert_eq!(parsed.skipped, vec![("u02".to_owned(), "chit-chat".to_owned())]);
    assert_eq!(parsed.ignored, vec!["skip:u07".to_owned(), "notes.txt".to_owned(), "wiki--Bad_Slug.md".to_owned()]);
}

#[test]
fn any_unknown_citation_title_or_size_fails_the_batch() {
    let none = parse_outputs(&[file("wiki--a.md", "# A\n[^u09]")], &labels(), &keys());
    assert_eq!(none.err(), Some(OutputError::UnknownCitation("u09".into())));
    // Old single-digit form, and a foreign k- key, are refused too.
    assert_eq!(parse_outputs(&[file("wiki--a.md", "# A\n[^u1]")], &labels(), &keys()).err(), Some(OutputError::UnknownCitation("u1".into())));
    assert_eq!(
        parse_outputs(&[file("wiki--a.md", "# A\n[^k-ffffffffffff]")], &labels(), &keys()).err(),
        Some(OutputError::UnknownCitation("k-ffffffffffff".into()))
    );
    assert!(matches!(parse_outputs(&[file("wiki--a.md", "no title")], &labels(), &keys()), Err(OutputError::BadTitle(_))));
    assert!(matches!(parse_outputs(&[file("wiki--a.md", &format!("# {}", "t".repeat(121)))], &labels(), &keys()), Err(OutputError::BadTitle(_))));
    let big = format!("# A\n{}", "x".repeat(MAX_PAGE_BYTES));
    assert!(matches!(parse_outputs(&[file("wiki--a.md", &big)], &labels(), &keys()), Err(OutputError::TooLarge(_))));
    let mut many = Vec::new();
    for index in 0..=MAX_PAGES_PER_BATCH {
        many.push(file(&format!("wiki--p{index}.md"), "# P"));
    }
    assert_eq!(parse_outputs(&many, &labels(), &keys()).err(), Some(OutputError::TooManyPages));
    assert_eq!(parse_outputs(&[file("knowledge-skipped.json", "{}")], &labels(), &keys()).err(), Some(OutputError::BadSkipJson));
    let invalid = GeneratedFile { name: "wiki--a.md".into(), bytes: vec![b'#', b' ', 0xff], sha256: String::new() };
    assert!(matches!(parse_outputs(&[invalid], &labels(), &keys()), Err(OutputError::NotUtf8(_))));
}

#[test]
fn unit_labels_are_rewritten_to_stable_keys() {
    let keys: BTreeMap<String, String> = [("u01".to_owned(), "aaaaaaaaaaaa".to_owned())].into_iter().collect();
    assert_eq!(
        rewrite_labels("A[^u01] B[^u01] C[^k-bbbbbbbbbbbb]", &keys),
        "A[^k-aaaaaaaaaaaa] B[^k-aaaaaaaaaaaa] C[^k-bbbbbbbbbbbb]"
    );
}

#[test]
fn the_status_line_never_carries_content() {
    let line = status_line(3, &Ok(AcceptOutcome::Accepted { pages: 2, cited: 5, skipped: 1, ignored: 0 }));
    assert_eq!(line, "Knowledge batch 3 accepted: 2 page(s) written, 5 unit(s) cited, 1 skipped, 0 output(s) ignored.");
    assert_eq!(
        status_line(4, &Err(PauseReason::InvalidOutput)),
        "Knowledge batch 4 not accepted (invalid_output); nothing was stored."
    );
}
