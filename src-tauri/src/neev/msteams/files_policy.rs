//! Pure publication policy; runnable with `rustc --test files_policy.rs`.
#[derive(Debug, PartialEq, Eq)]
pub enum FileError {
    InvalidName,
    MissingVersion,
    VersionConflict,
    InvalidIdentifier,
}

pub fn safe_name(value: &str) -> Result<(), FileError> {
    let upper = value.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    if value.is_empty()
        || value.len() > 200
        || value.trim() != value
        || value.ends_with('.')
        || value.starts_with("~$")
        || value
            .chars()
            .any(|c| c.is_control() || "\"*:<>?/\\|".contains(c))
        || [
            "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
            "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
            "LPT8", "LPT9",
        ]
        .contains(&stem)
        || upper == "DESKTOP.INI"
        || upper == ".LOCK"
        || upper.contains("_VTI_")
    {
        return Err(FileError::InvalidName);
    }
    Ok(())
}

pub fn segment(value: &str) -> Result<String, FileError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.len() > 1024
        || value.chars().any(char::is_control)
    {
        return Err(FileError::InvalidIdentifier);
    }
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(&mut encoded, "%{byte:02X}").unwrap();
        }
    }
    Ok(encoded)
}

pub fn check_version(expected: &str, current: &str) -> Result<(), FileError> {
    if expected.trim().is_empty()
        || current.trim().is_empty()
        || expected == "*"
        || current == "*"
        || expected.chars().any(char::is_control)
        || current.chars().any(char::is_control)
    {
        return Err(FileError::MissingVersion);
    }
    if expected != current {
        return Err(FileError::VersionConflict);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filenames_cannot_escape_folder_or_create_ambiguous_office_names() {
        for name in [
            "",
            ".",
            "..",
            "../proposal.docx",
            "a/b",
            "a\\b",
            "x\n.docx",
            " a.docx",
            "a.docx.",
            "a:docx",
            "CON",
            "desktop.ini",
            "~$proposal.docx",
        ] {
            assert_eq!(safe_name(name), Err(FileError::InvalidName), "{name:?}");
        }
        assert_eq!(safe_name("Annual proposal – 2026.docx"), Ok(()));
    }
    #[test]
    fn opaque_ids_are_encoded_as_one_segment() {
        assert_eq!(
            segment("19:abc@thread.tacv2").unwrap(),
            "19%3Aabc%40thread.tacv2"
        );
        assert_eq!(segment("a/b?#%").unwrap(), "a%2Fb%3F%23%25");
        assert_eq!(segment(".."), Err(FileError::InvalidIdentifier));
    }
    #[test]
    fn revision_requires_exact_nonempty_version() {
        assert_eq!(
            check_version("\"v1\"", "\"v2\""),
            Err(FileError::VersionConflict)
        );
        assert_eq!(check_version("", ""), Err(FileError::MissingVersion));
        assert_eq!(check_version("*", "*"), Err(FileError::MissingVersion));
        assert_eq!(check_version("\"v1\"", "\"v1\""), Ok(()));
    }
}
