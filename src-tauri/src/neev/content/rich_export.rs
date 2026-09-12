use super::{
    rich::{InlineRun, RichBlock, RichDocument, LIMIT},
    types::ContentError,
};

struct Output(String);
impl Output {
    fn push(&mut self, text: &str) -> Result<(), ContentError> {
        if text.len() > LIMIT - self.0.len() {
            return Err(ContentError::Invalid);
        }
        self.0.push_str(text);
        Ok(())
    }
    fn character(&mut self, c: char) -> Result<(), ContentError> {
        self.push(c.encode_utf8(&mut [0; 4]))
    }
    fn escaped(&mut self, text: &str, continuation: &str) -> Result<(), ContentError> {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if matches!(c, '\n' | '\r') {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                self.push("\n")?;
                self.push(continuation)?;
                continue;
            }
            if c.is_ascii_punctuation() {
                self.push("\\")?;
            }
            self.character(c)?;
        }
        Ok(())
    }
    fn repeat(&mut self, text: &str, count: usize) -> Result<(), ContentError> {
        for _ in 0..count {
            self.push(text)?;
        }
        Ok(())
    }
    fn destination(&mut self, href: &str) -> Result<(), ContentError> {
        // Keep URL delimiters from escaping their Markdown destination. The
        // validated original scheme is preserved; no HTML/autolinks are emitted.
        for c in href.chars() {
            match c {
                '(' => self.push("%28")?,
                ')' => self.push("%29")?,
                '<' => self.push("%3C")?,
                '>' => self.push("%3E")?,
                '"' => self.push("%22")?,
                '\'' => self.push("%27")?,
                '|' => self.push("%7C")?,
                '&' => self.push("\\&")?,
                _ => self.character(c)?,
            }
        }
        Ok(())
    }
    fn runs(
        &mut self,
        runs: &[InlineRun],
        table: bool,
        continuation: &str,
    ) -> Result<(), ContentError> {
        for run in runs {
            if run.text.is_empty() {
                continue;
            }
            if run.href.is_some() {
                self.push("[")?;
            }
            if run.styles.iter().any(|s| s == "code") {
                let fence = backticks(&run.text) + 1;
                self.repeat("`", fence)?;
                self.push(" ")?;
                for c in run.text.chars() {
                    if table && c == '|' {
                        self.push("\\")?;
                    }
                    self.character(if matches!(c, '\n' | '\r') { ' ' } else { c })?;
                }
                self.push(" ")?;
                self.repeat("`", fence)?;
            } else {
                for style in &run.styles {
                    self.push(marker(style))?;
                }
                self.escaped(&run.text, continuation)?;
                for style in run.styles.iter().rev() {
                    self.push(marker(style))?;
                }
            }
            if let Some(href) = &run.href {
                self.push("](")?;
                self.destination(href)?;
                self.push(")")?;
            }
        }
        Ok(())
    }
    fn blocks(&mut self, blocks: &[RichBlock], depth: usize) -> Result<(), ContentError> {
        let indent = "    ".repeat(depth);
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 && !same_list(&blocks[index - 1], block) {
                self.push("\n")?;
            }
            self.push(&indent)?;
            match block {
                RichBlock::Paragraph { content, .. } => self.runs(content, false, &indent)?,
                RichBlock::Heading { content, level, .. } => {
                    self.repeat("#", usize::from(*level))?;
                    self.push(" ")?;
                    self.runs(content, false, &indent)?;
                }
                RichBlock::BulletListItem { content, .. } => {
                    self.push("- ")?;
                    self.runs(content, false, &indent)?;
                }
                RichBlock::NumberedListItem { content, .. } => {
                    self.push("1. ")?;
                    self.runs(content, false, &indent)?;
                }
                RichBlock::CheckListItem {
                    content, checked, ..
                } => {
                    self.push(if *checked { "- [x] " } else { "- [ ] " })?;
                    self.runs(content, false, &indent)?;
                }
                RichBlock::Quote { content, .. } => {
                    self.push("> ")?;
                    self.runs(content, false, &format!("{indent}> "))?;
                }
                RichBlock::CodeBlock { text, language, .. } => {
                    let fence = 3.max(backticks(text) + 1);
                    self.repeat("`", fence)?;
                    self.push(language)?;
                    self.push("\n")?;
                    for line in text.split_inclusive('\n') {
                        self.push(&indent)?;
                        self.push(line)?;
                    }
                    if !text.ends_with('\n') {
                        self.push("\n")?;
                    }
                    self.push(&indent)?;
                    self.repeat("`", fence)?;
                }
                RichBlock::Table { rows, .. } => {
                    for (index, row) in rows.iter().enumerate() {
                        if index > 0 {
                            self.push(&indent)?;
                        }
                        self.push("| ")?;
                        for cell in row {
                            self.runs(cell, true, "")?;
                            self.push(" | ")?;
                        }
                        self.push("\n")?;
                        if index == 0 {
                            self.push(&indent)?;
                            self.push("|")?;
                            for _ in row {
                                self.push(" --- |")?;
                            }
                            self.push("\n")?;
                        }
                    }
                    // The table already ended its last row.
                    continue;
                }
            }
            self.push("\n")?;
            self.blocks(block.identity_children().1, depth + 1)?;
        }
        Ok(())
    }
}
fn same_list(a: &RichBlock, b: &RichBlock) -> bool {
    matches!(
        (a, b),
        (
            RichBlock::BulletListItem { .. },
            RichBlock::BulletListItem { .. }
        ) | (
            RichBlock::NumberedListItem { .. },
            RichBlock::NumberedListItem { .. }
        ) | (
            RichBlock::CheckListItem { .. },
            RichBlock::CheckListItem { .. }
        )
    )
}
fn marker(style: &str) -> &str {
    match style {
        "bold" => "**",
        "italic" => "*",
        "strike" => "~~",
        _ => "",
    }
}
fn backticks(text: &str) -> usize {
    let (mut longest, mut current) = (0, 0);
    for b in text.bytes() {
        if b == b'`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}
pub(super) fn render(document: &RichDocument) -> Result<String, ContentError> {
    let mut output = Output(String::new());
    output.blocks(&document.blocks, 0)?;
    Ok(output.0)
}
