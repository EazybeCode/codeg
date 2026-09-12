use super::types::ContentError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RichDocument {
    pub version: u8,
    pub blocks: Vec<RichBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InlineRun {
    pub text: String,
    pub styles: Vec<String>,
    #[serde(deserialize_with = "required_href")]
    pub href: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum RichBlock {
    #[serde(rename = "paragraph")]
    Paragraph {
        id: String,
        content: Vec<InlineRun>,
        children: Vec<RichBlock>,
    },
    #[serde(rename = "heading")]
    Heading {
        id: String,
        content: Vec<InlineRun>,
        children: Vec<RichBlock>,
        level: u8,
    },
    #[serde(rename = "bulletListItem")]
    BulletListItem {
        id: String,
        content: Vec<InlineRun>,
        children: Vec<RichBlock>,
    },
    #[serde(rename = "numberedListItem")]
    NumberedListItem {
        id: String,
        content: Vec<InlineRun>,
        children: Vec<RichBlock>,
    },
    #[serde(rename = "checkListItem")]
    CheckListItem {
        id: String,
        content: Vec<InlineRun>,
        children: Vec<RichBlock>,
        checked: bool,
    },
    #[serde(rename = "quote")]
    Quote {
        id: String,
        content: Vec<InlineRun>,
        children: Vec<RichBlock>,
    },
    #[serde(rename = "codeBlock")]
    CodeBlock {
        id: String,
        children: Vec<RichBlock>,
        text: String,
        language: String,
    },
    #[serde(rename = "table")]
    Table {
        id: String,
        children: Vec<RichBlock>,
        rows: Vec<Vec<Vec<InlineRun>>>,
    },
}

fn required_href<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

pub(super) const LIMIT: usize = 1_048_576;

impl RichBlock {
    pub(super) fn identity_children(&self) -> (&str, &[RichBlock]) {
        match self {
            Self::Paragraph { id, children, .. }
            | Self::Heading { id, children, .. }
            | Self::BulletListItem { id, children, .. }
            | Self::NumberedListItem { id, children, .. }
            | Self::CheckListItem { id, children, .. }
            | Self::Quote { id, children, .. }
            | Self::CodeBlock { id, children, .. }
            | Self::Table { id, children, .. } => (id, children),
        }
    }
}

fn text_valid(text: &str) -> bool {
    text.chars()
        .all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
}

fn link_valid(href: &str) -> bool {
    if href.is_empty() || href.len() > LIMIT || href.contains('\\') {
        return false;
    }
    // Collapse nested percent escapes on a stack. Each reduction removes two
    // bytes, so deeply nested %25 input cannot cause quadratic rescanning.
    let mut decoded = Vec::with_capacity(href.len());
    for byte in href.bytes() {
        decoded.push(byte);
        loop {
            let n = decoded.len();
            if n < 3 || decoded[n - 3] != b'%' {
                break;
            }
            let (Some(high), Some(low)) = (
                (decoded[n - 2] as char).to_digit(16),
                (decoded[n - 1] as char).to_digit(16),
            ) else {
                break;
            };
            decoded.truncate(n - 3);
            decoded.push((high * 16 + low) as u8);
        }
    }
    if std::str::from_utf8(&decoded).map_or(true, |text| {
        text.chars().any(|c| c.is_control() || c.is_whitespace())
    }) {
        return false;
    }
    if href.starts_with('#') {
        return href.len() > 1;
    }
    let Some((scheme, rest)) = href.split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.is_empty() || authority.contains('@'))
    {
        return false;
    }
    url::Url::parse(href).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn validate_runs(
    runs: &[InlineRun],
    count: &mut usize,
    bytes: &mut usize,
    table: bool,
) -> Result<(), ContentError> {
    *count = count.checked_add(runs.len()).ok_or(ContentError::Invalid)?;
    if *count > 10_000 {
        return Err(ContentError::Invalid);
    }
    for run in runs {
        *bytes = bytes
            .checked_add(run.text.len())
            .ok_or(ContentError::Invalid)?;
        if *bytes > LIMIT
            || !text_valid(&run.text)
            || (table && run.text.contains(['\n', '\r', '\u{2028}', '\u{2029}']))
        {
            return Err(ContentError::Invalid);
        }
        let mut previous = None;
        for style in &run.styles {
            let index = ["bold", "italic", "strike", "code"]
                .iter()
                .position(|allowed| allowed == style)
                .ok_or(ContentError::Invalid)?;
            if previous.is_some_and(|p| p >= index) || (index == 3 && run.styles.len() != 1) {
                return Err(ContentError::Invalid);
            }
            previous = Some(index);
        }
        if let Some(href) = &run.href {
            *bytes = bytes.checked_add(href.len()).ok_or(ContentError::Invalid)?;
            if *bytes > LIMIT || !link_valid(href) {
                return Err(ContentError::Invalid);
            }
        }
    }
    Ok(())
}

pub(super) fn validate(document: &RichDocument) -> Result<(), ContentError> {
    if document.version != 1 || document.blocks.is_empty() || document.blocks.len() > 1000 {
        return Err(ContentError::Invalid);
    }
    let mut ids = std::collections::HashSet::new();
    let (mut count, mut runs, mut bytes) = (0usize, 0usize, 0usize);
    // Iterative traversal checks depth before visiting children, including typed
    // values constructed by callers (which bypass serde's recursion limit).
    let mut pending: Vec<(&RichBlock, usize)> = document.blocks.iter().map(|b| (b, 1)).collect();
    while let Some((block, depth)) = pending.pop() {
        count += 1;
        if count > 1000 || depth > 8 {
            return Err(ContentError::Invalid);
        }
        let (id, children) = block.identity_children();
        if !uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) || !ids.insert(id) {
            return Err(ContentError::Invalid);
        }
        if !children.is_empty() && (depth == 8 || children.len() > 1000 - count) {
            return Err(ContentError::Invalid);
        }
        match block {
            RichBlock::Paragraph { content, .. }
            | RichBlock::BulletListItem { content, .. }
            | RichBlock::NumberedListItem { content, .. }
            | RichBlock::CheckListItem { content, .. }
            | RichBlock::Quote { content, .. } => {
                validate_runs(content, &mut runs, &mut bytes, false)?
            }
            RichBlock::Heading { content, level, .. } => {
                if !(1..=3).contains(level) {
                    return Err(ContentError::Invalid);
                }
                validate_runs(content, &mut runs, &mut bytes, false)?;
            }
            RichBlock::CodeBlock { text, language, .. } => {
                bytes = bytes.checked_add(text.len()).ok_or(ContentError::Invalid)?;
                if !children.is_empty()
                    || bytes > LIMIT
                    || !text_valid(text)
                    || language.len() > 32
                    || !language
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-'))
                {
                    return Err(ContentError::Invalid);
                }
            }
            RichBlock::Table { rows, .. } => {
                let columns = rows.first().map_or(0, Vec::len);
                if !children.is_empty() || rows.len() > 100 || !(1..=20).contains(&columns) {
                    return Err(ContentError::Invalid);
                }
                for row in rows {
                    if row.len() != columns {
                        return Err(ContentError::Invalid);
                    }
                    for cell in row {
                        validate_runs(cell, &mut runs, &mut bytes, true)?;
                    }
                }
            }
        }
        pending.extend(children.iter().map(|child| (child, depth + 1)));
    }
    Ok(())
}

struct BoundedBytes(Vec<u8>);
impl std::io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > LIMIT - self.0.len() {
            return Err(std::io::Error::other("rich document too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode(document: &RichDocument) -> Result<Vec<u8>, ContentError> {
    validate(document)?;
    let mut bytes = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut bytes, document).map_err(|_| ContentError::Invalid)?;
    super::rich_export::render(document)?;
    Ok(bytes.0)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<RichDocument, ContentError> {
    if bytes.len() > LIMIT {
        return Err(ContentError::Invalid);
    }
    let document: RichDocument =
        serde_json::from_slice(bytes).map_err(|_| ContentError::Invalid)?;
    encode(&document)?;
    Ok(document)
}
pub(crate) fn export_markdown(document: &RichDocument) -> Result<String, ContentError> {
    validate(document)?;
    // Export accepts exactly the same bounded envelope contract as persistence.
    let mut bytes = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut bytes, document).map_err(|_| ContentError::Invalid)?;
    super::rich_export::render(document)
}
