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
fn rich_supports_divider_blocks() {
    let para = |id: &str, text: &str| RichBlock::Paragraph {
        id: id.into(),
        content: vec![InlineRun {
            text: text.into(),
            styles: vec![],
            href: None,
        }],
        children: vec![],
    };
    let doc = RichDocument {
        version: 1,
        blocks: vec![
            para("11111111-1111-4111-8111-111111111111", "Above"),
            RichBlock::Divider {
                id: "22222222-2222-4222-8222-222222222222".into(),
                children: vec![],
            },
            para("33333333-3333-4333-8333-333333333333", "Below"),
        ],
    };
    let bytes = rich::encode(&doc).unwrap();
    assert_eq!(rich::decode(&bytes).unwrap(), doc);
    assert_eq!(
        rich::export_markdown(&doc).unwrap(),
        "Above\n\n---\n\nBelow\n"
    );
    // A divider carries no children.
    let mut bad = doc.clone();
    if let RichBlock::Divider { children, .. } = &mut bad.blocks[1] {
        children.push(para("44444444-4444-4444-8444-444444444444", "x"));
    }
    assert_eq!(rich::encode(&bad), Err(ContentError::Invalid));
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

// Parse the actual Rust export with the existing application Markdown parser.
// Only bounded invented Markdown enters stdin; no database/provider environment
// is inherited. Missing tooling, parser errors, invalid JSON, or timeout fail.
fn parsed_export(document: &RichDocument) -> (String, serde_json::Value) {
    use std::{
        io::{Read, Write},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let markdown = rich::export_markdown(document).unwrap();
    assert!(markdown.len() < 16_000);
    let script = r#"import fs from 'node:fs';
import {fromMarkdown} from 'mdast-util-from-markdown';
import gfm from 'remark-gfm';
const data={}; gfm.call({data:()=>data});
const tree=fromMarkdown(fs.readFileSync(0,'utf8'),{extensions:data.micromarkExtensions,mdastExtensions:data.fromMarkdownExtensions});
process.stdout.write(JSON.stringify(tree,(key,value)=>key==='position'?undefined:value));"#;
    let mut child = Command::new("node")
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").expect("Node toolchain PATH"),
        )
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .args(["--input-type=module", "-e", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("installed Node Markdown parser");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(markdown.as_bytes())
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(16_001).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(16_001).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Markdown parser timed out for {markdown:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = out.join().unwrap();
    let stderr = err.join().unwrap();
    assert!(
        status.success(),
        "Markdown parser failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stdout.len() <= 16_000 && stderr.len() <= 16_000);
    let tree = serde_json::from_slice(&stdout).expect("valid Markdown AST response");
    (markdown, tree)
}

fn text_styles(node: &serde_json::Value, active: u8, result: &mut Vec<(char, u8)>) {
    let bit = match node["type"].as_str().unwrap() {
        "strong" => 1,
        "emphasis" => 2,
        "delete" => 4,
        "inlineCode" => 8,
        _ => 0,
    };
    if let Some(text) = node["value"].as_str() {
        assert_ne!(node["type"], "html", "export must not create HTML");
        result.extend(text.chars().map(|c| (c, active | bit)));
    }
    if let Some(children) = node["children"].as_array() {
        for child in children {
            text_styles(child, active | bit, result);
        }
    }
}

fn with_runs(runs: Vec<InlineRun>) -> RichDocument {
    let mut doc = document();
    if let RichBlock::Paragraph { content, .. } = &mut doc.blocks[0] {
        *content = runs;
    }
    doc
}
fn styled(text: &str, bits: u8) -> InlineRun {
    InlineRun {
        text: text.into(),
        styles: [("bold", 1), ("italic", 2), ("strike", 4), ("code", 8)]
            .into_iter()
            .filter(|(_, bit)| bits & bit != 0)
            .map(|(style, _)| style.into())
            .collect(),
        href: None,
    }
}

#[test]
fn rich_export_parser_preserves_style_whitespace_and_adjacent_runs() {
    for (runs, want) in [
        (
            vec![styled(" Hello ", 1)],
            " Hello ".chars().map(|c| (c, 1)).collect::<Vec<_>>(),
        ),
        (
            vec![styled("A", 1), styled("B", 1)],
            vec![('A', 1), ('B', 1)],
        ),
        (
            vec![styled("before", 0), styled("middle", 2), styled("after", 0)],
            "before"
                .chars()
                .map(|c| (c, 0))
                .chain("middle".chars().map(|c| (c, 2)))
                .chain("after".chars().map(|c| (c, 0)))
                .collect(),
        ),
        (
            vec![styled(" ", 8), styled(" ", 8)],
            vec![(' ', 8), (' ', 8)],
        ),
        (
            vec![styled(" ` &amp; ", 8), styled("&#32; `` ", 8)],
            " ` &amp; &#32; `` ".chars().map(|c| (c, 8)).collect(),
        ),
    ] {
        let (markdown, tree) = parsed_export(&with_runs(runs));
        let mut got = Vec::new();
        text_styles(&tree, 0, &mut got);
        assert_eq!(got, want, "actual export: {markdown:?}; AST: {tree}");
    }
}

#[test]
fn rich_export_parser_preserves_all_emphasis_transitions() {
    for left in 0..8 {
        for right in 0..8 {
            let (markdown, tree) =
                parsed_export(&with_runs(vec![styled("A", left), styled("B", right)]));
            let mut got = Vec::new();
            text_styles(&tree, 0, &mut got);
            assert_eq!(
                got,
                vec![('A', left), ('B', right)],
                "styles {left}->{right}; actual export: {markdown:?}; AST: {tree}"
            );
        }
    }
}

#[test]
fn rich_export_parser_keeps_blank_line_paragraphs_inside_list_items() {
    for (kind, bits) in [
        ("bullet", 0),
        ("numbered", 0),
        ("checked", 0),
        ("bullet", 7),
        ("numbered", 7),
        ("checked", 7),
    ] {
        let id = "11111111-1111-4111-8111-111111111111".into();
        let content = vec![styled("first\n\nsecond", bits)];
        let children = vec![];
        let block = match kind {
            "bullet" => RichBlock::BulletListItem {
                id,
                content,
                children,
            },
            "numbered" => RichBlock::NumberedListItem {
                id,
                content,
                children,
            },
            _ => RichBlock::CheckListItem {
                id,
                content,
                children,
                checked: true,
            },
        };
        let (markdown, tree) = parsed_export(&RichDocument {
            version: 1,
            blocks: vec![block],
        });
        assert_eq!(
            tree["children"].as_array().unwrap().len(),
            1,
            "{kind}: {markdown:?}; AST: {tree}"
        );
        let list = &tree["children"][0];
        assert_eq!(list["type"], "list");
        assert_eq!(list["ordered"], kind == "numbered");
        let item = &list["children"][0];
        assert_eq!(list["children"].as_array().unwrap().len(), 1);
        assert_eq!(item["type"], "listItem");
        if kind == "checked" {
            assert_eq!(item["checked"], true);
        }
        assert_eq!(
            item["children"].as_array().unwrap().len(),
            2,
            "{kind}: {markdown:?}; AST: {tree}"
        );
        for (index, want) in ["first", "second"].into_iter().enumerate() {
            let paragraph = &item["children"][index];
            assert_eq!(paragraph["type"], "paragraph");
            let mut chars = Vec::new();
            text_styles(paragraph, 0, &mut chars);
            assert_eq!(
                chars,
                want.chars().map(|c| (c, bits)).collect::<Vec<_>>(),
                "{kind}, styles {bits}: {markdown:?}; AST: {tree}"
            );
        }
    }
}
