//! Deterministic snapshot units (Amendment 1 §A1.3, B1/B6).
//!
//! Every unit is at most `MAX_UNIT_BYTES`, header included (header at most
//! `MAX_UNIT_HEADER_BYTES`). The same input always gives the same units.
//!
//! - Documents split at the nearest heading before the limit, else a blank
//!   line, else a newline, else a char boundary marked `[continued]`. No
//!   character of the source is ever dropped: concatenating the bodies (minus
//!   the `[continued]` markers) gives the document back.
//! - Chats pack whole messages greedily. A segment's header depends only on
//!   messages before it, so a closed segment never changes as messages arrive.

use super::types::{MAX_UNIT_BYTES, MAX_UNIT_HEADER_BYTES};

/// One unit. `segment` is 1-based; `text` includes its header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unit {
    pub segment: i32,
    pub from_sequence: Option<i64>,
    pub to_sequence: Option<i64>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub sequence: i64,
    pub at: i64,
    pub who: String,
    pub body: String,
    /// Attachment file names (contents are never captured).
    pub files: Vec<String>,
}

pub(crate) const CONTINUED: &str = "\n[continued]\n";
pub(crate) const TRUNCATED: &str = "\n[message truncated]\n";

/// UTC ISO-8601 seconds, e.g. `2023-11-14T22:13:20Z`.
pub(crate) fn iso(at: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(at, 0)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

/// Clip to `max` bytes on a char boundary, with control characters (newlines
/// included) flattened, so a header field can never forge another field.
fn field(value: &str, max: usize) -> String {
    let flat: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    clip(&flat, max).to_owned()
}

fn clip(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        Some((hashes, line[hashes..].trim()))
    } else {
        None
    }
}

/// The heading breadcrumb in effect at byte `position` (headings strictly before it).
fn breadcrumb(markdown: &str, position: usize) -> String {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut offset = 0usize;
    for line in markdown.split_inclusive('\n') {
        if offset >= position {
            break;
        }
        if let Some((level, text)) = heading(line.trim_end_matches(['\n', '\r'])) {
            while stack.last().is_some_and(|(l, _)| *l >= level) {
                stack.pop();
            }
            stack.push((level, text.to_owned()));
        }
        offset += line.len();
    }
    stack
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Where to cut `markdown[start..]` so the chunk is at most `budget` bytes,
/// and whether the cut is a hard (mid-line) cut.
fn cut(markdown: &str, start: usize, budget: usize) -> (usize, bool) {
    let rest = &markdown[start..];
    if rest.len() <= budget {
        return (markdown.len(), false);
    }
    let window = clip(rest, budget);
    // 1. The last heading line start inside the window (not at offset 0).
    let mut best_heading = None;
    let mut offset = 0usize;
    for line in window.split_inclusive('\n') {
        if offset > 0 && heading(line.trim_end_matches(['\n', '\r'])).is_some() {
            // A heading line that started inside the window and is complete.
            best_heading = Some(offset);
        }
        offset += line.len();
    }
    if let Some(at) = best_heading {
        return (start + at, false);
    }
    // 2. After the last blank line.
    if let Some(at) = window.rfind("\n\n") {
        if at + 2 > 0 {
            return (start + at + 2, false);
        }
    }
    // 3. After the last newline.
    if let Some(at) = window.rfind('\n') {
        return (start + at + 1, false);
    }
    // 4. A char-boundary cut, leaving room for the `[continued]` marker.
    let hard = clip(rest, budget.saturating_sub(CONTINUED.len()));
    (start + hard.len().max(1).min(rest.len()), true)
}

fn doc_header(title: &str, section: &str, k: usize, n: usize) -> String {
    let title = field(title, 300);
    let section = field(section, 500);
    format!("# Source: {title}\nsection: {section}\nsegment: {k} of {n}\n\n")
}

/// Segment a document. Empty (or whitespace-only) content gives no unit.
pub(crate) fn doc_segments(title: &str, markdown: &str) -> Vec<Unit> {
    if markdown.trim().is_empty() {
        return Vec::new();
    }
    let budget = MAX_UNIT_BYTES - MAX_UNIT_HEADER_BYTES;
    let mut chunks: Vec<(usize, usize, bool)> = Vec::new();
    let mut start = 0usize;
    while start < markdown.len() {
        let (end, hard) = cut(markdown, start, budget);
        let end = if end <= start {
            // Never stall: at least one character.
            let mut next = start + 1;
            while next < markdown.len() && !markdown.is_char_boundary(next) {
                next += 1;
            }
            next
        } else {
            end
        };
        chunks.push((start, end, hard));
        start = end;
    }
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, (from, to, hard))| {
            let header = doc_header(title, &breadcrumb(markdown, from), index + 1, n);
            let mut text = header;
            text.push_str(&markdown[from..to]);
            if hard {
                text.push_str(CONTINUED);
            }
            debug_assert!(text.len() <= MAX_UNIT_BYTES);
            Unit {
                segment: (index + 1) as i32,
                from_sequence: None,
                to_sequence: None,
                text,
            }
        })
        .collect()
}

fn chat_header(title: &str, k: usize, previous: Option<&ChatMessage>) -> String {
    let title = field(title, 300);
    match previous {
        None => format!("# Source: {title} (team chat)\nsegment: {k}\n\n"),
        Some(message) => format!(
            "# Source: {title} (team chat)\nsegment: {k}\nprevious: {} · {}\n\n",
            field(&message.who, 300),
            iso(message.at)
        ),
    }
}

/// One message as rendered into a chat unit, bounded so it always fits a
/// unit with its header (stored bodies are at most 32 KiB, so truncation is
/// only a totality guard).
fn render_message(message: &ChatMessage) -> String {
    let mut files = String::new();
    let mut names = message.files.clone();
    names.sort();
    for name in names {
        let line = format!("[file: {}]\n", field(&name, 200));
        // File names are listed, never captured; the list itself is bounded.
        if files.len() + line.len() > 8 * 1024 {
            break;
        }
        files.push_str(&line);
    }
    let head = format!("### {} · {}\n\n", field(&message.who, 300), iso(message.at));
    let room = (MAX_UNIT_BYTES - MAX_UNIT_HEADER_BYTES)
        .saturating_sub(head.len())
        .saturating_sub(files.len())
        .saturating_sub(2);
    let body = if message.body.len() > room {
        let mut clipped = clip(&message.body, room.saturating_sub(TRUNCATED.len())).to_owned();
        clipped.push_str(TRUNCATED);
        clipped
    } else {
        message.body.clone()
    };
    format!("{head}{body}\n{files}\n")
}

/// Segment a chat (messages in sequence order). No messages gives no unit.
pub(crate) fn chat_segments(title: &str, messages: &[ChatMessage]) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut current: Option<(String, i64, i64)> = None;
    let mut previous: Option<&ChatMessage> = None;
    for (index, message) in messages.iter().enumerate() {
        let rendered = render_message(message);
        if let Some((text, from, to)) = current.as_mut() {
            if text.len() + rendered.len() <= MAX_UNIT_BYTES {
                text.push_str(&rendered);
                *to = message.sequence;
                continue;
            }
            units.push(Unit {
                segment: (units.len() + 1) as i32,
                from_sequence: Some(*from),
                to_sequence: Some(*to),
                text: std::mem::take(text),
            });
            previous = Some(&messages[index - 1]);
        }
        let mut text = chat_header(title, units.len() + 1, previous);
        text.push_str(&rendered);
        current = Some((text, message.sequence, message.sequence));
    }
    if let Some((text, from, to)) = current {
        units.push(Unit {
            segment: (units.len() + 1) as i32,
            from_sequence: Some(from),
            to_sequence: Some(to),
            text,
        });
    }
    units
}

/// The body of a document unit (header and `[continued]` removed). Tests use
/// it to prove nothing is dropped.
#[cfg(test)]
pub(crate) fn doc_body(unit: &Unit) -> &str {
    let text = unit.text.as_str();
    let body = text.splitn(2, "\n\n").nth(1).unwrap_or("");
    body.strip_suffix(CONTINUED).unwrap_or(body)
}
