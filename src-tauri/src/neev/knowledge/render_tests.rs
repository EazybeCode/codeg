//! Task 6 (A1.6): deterministic segmentation.

use super::render::*;
use super::types::{MAX_UNIT_BYTES, MAX_UNIT_HEADER_BYTES};

fn big_document() -> String {
    // ~1 MiB with headings, paragraphs and long lines, deterministic.
    let mut text = String::new();
    let mut n = 0u64;
    while text.len() < 1_048_576 - 4_096 {
        n += 1;
        if n % 40 == 1 {
            text.push_str(&format!("# Chapter {n}\n\n"));
        } else if n % 10 == 1 {
            text.push_str(&format!("## Section {n}\n\n"));
        }
        text.push_str(&format!(
            "Paragraph {n}: {} ünïcødé ✓\n\n",
            "lorem ipsum dolor sit amet ".repeat((n % 17 + 1) as usize)
        ));
    }
    text
}

#[test]
fn a_one_mebibyte_document_is_fully_covered_by_bounded_units_identically_on_rerun() {
    let markdown = big_document();
    let units = doc_segments("Handbook", &markdown);
    assert!(units.len() >= 16, "{} units", units.len());
    let n = units.len();
    for (index, unit) in units.iter().enumerate() {
        assert!(unit.text.len() <= MAX_UNIT_BYTES, "unit {index} is {} bytes", unit.text.len());
        let header_end = unit.text.find("\n\n").unwrap() + 2;
        assert!(header_end <= MAX_UNIT_HEADER_BYTES);
        assert!(unit.text.starts_with("# Source: Handbook\nsection: "));
        assert!(unit.text[..header_end].contains(&format!("segment: {} of {n}\n", index + 1)));
        assert_eq!(unit.segment, (index + 1) as i32);
    }
    // Nothing dropped: the bodies concatenate back to the document.
    let rebuilt: String = units.iter().map(doc_body).collect();
    assert_eq!(rebuilt, markdown);
    // Splits prefer headings: some unit (after the first) starts at a heading,
    // and its breadcrumb names the enclosing chapter.
    assert!(units[1..].iter().any(|u| doc_body(u).starts_with('#')));
    assert!(units[1..].iter().any(|u| u.text.contains("section: Chapter ")));
    // Deterministic.
    assert_eq!(doc_segments("Handbook", &markdown), units);
}

#[test]
fn an_unbreakable_line_is_cut_on_a_char_boundary_and_marked_continued() {
    let markdown = "é".repeat(100_000); // 200 KB, no newline at all
    let units = doc_segments("Blob", &markdown);
    assert!(units.len() >= 4);
    assert!(units.iter().all(|u| u.text.len() <= MAX_UNIT_BYTES));
    assert!(units[..units.len() - 1].iter().all(|u| u.text.ends_with(CONTINUED)));
    assert!(!units.last().unwrap().text.ends_with(CONTINUED));
    let rebuilt: String = units.iter().map(doc_body).collect();
    assert_eq!(rebuilt, markdown);
}

#[test]
fn empty_documents_and_chats_give_no_units() {
    assert!(doc_segments("Empty", "").is_empty());
    assert!(doc_segments("Blank", "\n  \n").is_empty());
    assert!(chat_segments("Quiet", &[]).is_empty());
}

fn message(sequence: i64, body: &str) -> ChatMessage {
    ChatMessage {
        sequence,
        at: 1_700_000_000 + sequence,
        who: format!("Person {} (person)", sequence % 3),
        body: body.into(),
        files: if sequence % 5 == 0 { vec!["report.pdf".into(), "a.png".into()] } else { vec![] },
    }
}

#[test]
fn closed_chat_segments_stay_byte_stable_as_messages_are_added() {
    let body = "x".repeat(20_000);
    let first: Vec<_> = (1..=12).map(|i| message(i, &body)).collect();
    let units = chat_segments("Team", &first);
    assert!(units.len() >= 3);
    for unit in &units {
        assert!(unit.text.len() <= MAX_UNIT_BYTES);
    }
    assert_eq!(units[0].from_sequence, Some(1));
    for pair in units.windows(2) {
        assert_eq!(pair[0].to_sequence.unwrap() + 1, pair[1].from_sequence.unwrap());
    }
    assert!(units[0].text.starts_with("# Source: Team (team chat)\nsegment: 1\n\n"));
    assert!(units[1].text.contains("\nprevious: Person "));
    // Attachments by name only.
    assert!(units.iter().any(|u| u.text.contains("[file: a.png]\n[file: report.pdf]")));
    // More messages: every closed segment is byte-identical; only the open
    // (last) one may change.
    let more: Vec<_> = (1..=20).map(|i| message(i, &body)).collect();
    let grown = chat_segments("Team", &more);
    assert!(grown.len() > units.len());
    assert_eq!(&grown[..units.len() - 1], &units[..units.len() - 1]);
}

#[test]
fn a_message_too_large_for_a_unit_is_truncated_not_dropped() {
    let units = chat_segments("Team", &[message(1, &"y".repeat(200_000))]);
    assert_eq!(units.len(), 1);
    assert!(units[0].text.len() <= MAX_UNIT_BYTES);
    assert!(units[0].text.contains(TRUNCATED));
}
