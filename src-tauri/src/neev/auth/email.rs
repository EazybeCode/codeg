/// Conservative canonicalization shared by sign-in and invitations.
pub(crate) fn canonical_email(input: &str) -> Option<String> {
    if !input.is_ascii() || input.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return None;
    }
    let value = input.trim();
    if value.len() > 254 {
        return None;
    }
    let (local, domain) = value.split_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
    {
        return None;
    }
    if !domain.contains('.')
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_case_and_whitespace_without_rewriting_identity() {
        assert_eq!(
            canonical_email(" SUPPORT@NEEV.SO "),
            Some("support@neev.so".into())
        );
        assert_eq!(
            canonical_email("first.last+campaign@Example.test"),
            Some("first.last+campaign@example.test".into())
        );
    }
    #[test]
    fn rejects_headers_display_names_and_invalid_addresses() {
        for email in [
            "",
            "a",
            "@neev.so",
            "a@",
            "a@@neev.so",
            "Name <a@neev.so>",
            "a@neev.so\r\nBcc:b@evil.test",
            "a b@neev.so",
            "a@-neev.so",
            ".a@neev.so",
            "a..b@neev.so",
            "a@neev..so",
        ] {
            assert_eq!(canonical_email(email), None, "{email:?}");
        }
        assert_eq!(
            canonical_email(&format!("{}@neev.so", "a".repeat(65))),
            None
        );
    }
}
