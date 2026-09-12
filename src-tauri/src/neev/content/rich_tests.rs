use super::{
    rich::{self, InlineRun, RichBlock, RichDocument},
    types::ContentError,
};

pub(super) const SOURCE: &[u8] = br##"{"version":1,"blocks":[{"type":"paragraph","id":"11111111-1111-4111-8111-111111111111","content":[{"text":"Hello","styles":[],"href":null}],"children":[]}]}"##;
pub(super) fn document() -> RichDocument {
    RichDocument {
        version: 1,
        blocks: vec![RichBlock::Paragraph {
            id: "11111111-1111-4111-8111-111111111111".into(),
            content: vec![InlineRun {
                text: "Hello".into(),
                styles: vec![],
                href: None,
            }],
            children: vec![],
        }],
    }
}
#[test]
fn rich_canonical_roundtrip_and_export() {
    assert_eq!(
        rich::decode(SOURCE).expect("supported rich document"),
        document()
    );
    assert_eq!(rich::encode(&document()).unwrap(), SOURCE);
    assert_eq!(rich::export_markdown(&document()).unwrap(), "Hello\n");
}
#[test]
fn rich_rejects_unknown_or_unsafe_contracts() {
    let base = String::from_utf8(SOURCE.to_vec()).unwrap();
    for (from, to) in [
        ("\"version\":1", "\"version\":2"),
        ("paragraph", "image"),
        ("\"content\":", "\"unknown\":true,\"content\":"),
        ("\"href\":null", "\"href\":\"javascript:alert(1)\""),
        ("11111111-1111-4111-8111-111111111111", "bad-id"),
        ("\"styles\":[]", "\"styles\":[\"bold\",\"bold\"]"),
    ] {
        assert_eq!(
            rich::decode(base.replace(from, to).as_bytes()),
            Err(ContentError::Invalid),
            "{to}"
        );
    }
    let mut duplicate = document();
    duplicate.blocks.push(duplicate.blocks[0].clone());
    assert_eq!(rich::encode(&duplicate), Err(ContentError::Invalid));
    let mut deep = document();
    for i in 2..=9 {
        deep.blocks = vec![RichBlock::Paragraph {
            id: format!("11111111-1111-4111-8111-{i:012}"),
            content: vec![],
            children: deep.blocks,
        }];
    }
    assert_eq!(rich::encode(&deep), Err(ContentError::Invalid));
    let mut huge = document();
    if let RichBlock::Paragraph { content, .. } = &mut huge.blocks[0] {
        content[0].text = "*".repeat(600_000);
    }
    assert_eq!(rich::export_markdown(&huge), Err(ContentError::Invalid));
    let table = RichDocument {
        version: 1,
        blocks: vec![RichBlock::Table {
            id: "11111111-1111-4111-8111-111111111111".into(),
            children: vec![],
            rows: vec![vec![]],
        }],
    };
    assert_eq!(rich::encode(&table), Err(ContentError::Invalid));
}

#[test]
fn rich_validates_links_styles_cells_and_required_fields() {
    for href in [
        "https://example.test/path#part",
        "http://example.test",
        "#section",
    ] {
        let mut doc = document();
        if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
            content[0].href = Some(href.into());
        }
        assert!(rich::encode(&doc).is_ok(), "{href}");
    }
    for href in [
        "javascript:alert(1)",
        "https:example.test",
        "https://@example.test",
        "https://user:secret@example.test",
        "https:///",
        "//example.test",
        "https://example.test/%0a",
        "https://example.test/%250d",
        "https://example.test/a b",
        "#a\nb",
    ] {
        let mut doc = document();
        if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
            content[0].href = Some(href.into());
        }
        assert_eq!(rich::encode(&doc), Err(ContentError::Invalid), "{href}");
    }
    let mut nested = document();
    if let RichBlock::Paragraph { content, .. } = &mut nested.blocks[0] {
        content[0].href = Some(format!("https://example.test/%{}61", "25".repeat(100)));
    }
    assert!(rich::encode(&nested).is_ok());
    if let RichBlock::Paragraph { content, .. } = &mut nested.blocks[0] {
        content[0].href = Some("https://example.test/%25%30%61".into());
    }
    assert_eq!(rich::encode(&nested), Err(ContentError::Invalid));
    for styles in [
        vec!["italic", "bold"],
        vec!["code", "bold"],
        vec!["color"],
        vec!["strike", "strike"],
    ] {
        let mut doc = document();
        if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
            content[0].styles = styles.into_iter().map(str::to_owned).collect();
        }
        assert_eq!(rich::encode(&doc), Err(ContentError::Invalid));
    }
    let source = String::from_utf8(SOURCE.to_vec()).unwrap();
    for changed in [
        source.replace(",\"href\":null", ""),
        source.replace("Hello", "bad\\u0000"),
        source.replace("\"children\":[]", "\"children\":[],\"level\":2"),
    ] {
        assert_eq!(rich::decode(changed.as_bytes()), Err(ContentError::Invalid));
    }
}

#[test]
fn rich_export_escapes_text_and_fences_code() {
    let source = String::from_utf8(SOURCE.to_vec())
        .unwrap()
        .replace("Hello", "# hi <script> & [x](y)");
    assert_eq!(
        rich::export_markdown(&rich::decode(source.as_bytes()).unwrap()).unwrap(),
        "\\# hi \\<script\\> \\& \\[x\\]\\(y\\)\n"
    );
    let code = RichDocument {
        version: 1,
        blocks: vec![RichBlock::CodeBlock {
            id: "11111111-1111-4111-8111-111111111111".into(),
            children: vec![],
            text: "```\n<script>".into(),
            language: "rust".into(),
        }],
    };
    assert_eq!(
        rich::export_markdown(&code).unwrap(),
        "````rust\n```\n<script>\n````\n"
    );
}

#[test]
fn rich_export_preserves_paragraphs_child_lines_and_url_literals() {
    let mut doc = document();
    let mut second = doc.blocks[0].clone();
    if let RichBlock::Paragraph { id, content, .. } = &mut second {
        *id = "22222222-2222-4222-8222-222222222222".into();
        content[0].text = "World".into();
    }
    doc.blocks.push(second);
    assert_eq!(rich::export_markdown(&doc).unwrap(), "Hello\n\nWorld\n");
    let mut child = document().blocks.remove(0);
    if let RichBlock::Paragraph { id, content, .. } = &mut child {
        *id = "22222222-2222-4222-8222-222222222222".into();
        content[0].text = "Child\nsecond".into();
    }
    let nested = RichDocument {
        version: 1,
        blocks: vec![RichBlock::BulletListItem {
            id: "11111111-1111-4111-8111-111111111111".into(),
            content: vec![InlineRun {
                text: "Parent".into(),
                styles: vec![],
                href: None,
            }],
            children: vec![child],
        }],
    };
    assert_eq!(
        rich::export_markdown(&nested).unwrap(),
        "- Parent\n    Child\n    second\n"
    );
    let mut link = document();
    if let RichBlock::Paragraph { content, .. } = &mut link.blocks[0] {
        content[0].href = Some("https://example.test/?a=1&NewLine;b=2".into());
    }
    assert_eq!(
        rich::export_markdown(&link).unwrap(),
        "[Hello](https://example.test/?a=1\\&NewLine;b=2)\n"
    );
}

#[test]
fn rich_enforces_collection_and_variant_limits() {
    let mut doc = document();
    if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
        content[0].text = "\t".repeat(600_000);
    }
    assert_eq!(rich::encode(&doc), Err(ContentError::Invalid));
    let mut doc = document();
    if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
        *content = vec![
            InlineRun {
                text: "a".into(),
                styles: vec![],
                href: None
            };
            10_000
        ];
    }
    assert!(rich::encode(&doc).is_ok());
    if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
        content.push(content[0].clone());
    }
    assert_eq!(rich::encode(&doc), Err(ContentError::Invalid));
    let mut doc = RichDocument {
        version: 1,
        blocks: (1..=1000)
            .map(|id| RichBlock::Paragraph {
                id: format!("11111111-1111-4111-8111-{id:012}"),
                content: vec![],
                children: vec![],
            })
            .collect(),
    };
    assert!(rich::encode(&doc).is_ok());
    doc.blocks.push(doc.blocks[0].clone());
    assert_eq!(rich::encode(&doc), Err(ContentError::Invalid));
    let mut deep = document();
    for id in 2..=8 {
        deep.blocks = vec![RichBlock::Paragraph {
            id: format!("11111111-1111-4111-8111-{id:012}"),
            content: vec![],
            children: deep.blocks,
        }];
    }
    assert!(rich::encode(&deep).is_ok());
    let mut table = RichDocument {
        version: 1,
        blocks: vec![RichBlock::Table {
            id: "11111111-1111-4111-8111-111111111111".into(),
            children: vec![],
            rows: vec![vec![vec![]; 20]; 100],
        }],
    };
    assert!(rich::encode(&table).is_ok());
    if let RichBlock::Table { rows, .. } = &mut table.blocks[0] {
        rows.push(rows[0].clone());
    }
    assert_eq!(rich::encode(&table), Err(ContentError::Invalid));
    for json in [
        r#"{"type":"heading","id":"11111111-1111-4111-8111-111111111111","content":[],"children":[],"level":4}"#,
        r#"{"type":"codeBlock","id":"11111111-1111-4111-8111-111111111111","text":"code","children":[],"language":"a/b"}"#,
        r#"{"type":"table","id":"11111111-1111-4111-8111-111111111111","children":[],"rows":[[[],[]],[[]]]}"#,
        r#"{"type":"table","id":"11111111-1111-4111-8111-111111111111","children":[],"rows":[[[{"text":"line\nbreak","styles":[],"href":null}]]]}"#,
    ] {
        assert_eq!(
            rich::decode(format!("{{\"version\":1,\"blocks\":[{json}]}}").as_bytes()),
            Err(ContentError::Invalid)
        );
    }
}
