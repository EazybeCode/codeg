use crate::neev::error::AuthError;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
#[derive(Clone, Copy)]
pub(crate) enum TokenPurpose {
    Session,
    Csrf,
    Browser,
    OAuth,
    Magic,
    Invite,
}
impl TokenPurpose {
    fn prefix(self) -> &'static str {
        match self {
            Self::Session => "n1.session.",
            Self::Csrf => "n1.csrf.",
            Self::Browser => "n1.browser.",
            Self::OAuth => "n1.oauth.",
            Self::Magic => "n1.magic.",
            Self::Invite => "n1.invite.",
        }
    }
}
pub(crate) fn random_token(purpose: TokenPurpose) -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| AuthError::Unavailable)?;
    Ok(format!(
        "{}{}",
        purpose.prefix(),
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}
pub(crate) fn digest(purpose: TokenPurpose, token: &str) -> Result<String, AuthError> {
    let raw = token
        .strip_prefix(purpose.prefix())
        .ok_or(AuthError::Invalid)?;
    if raw.len() != 43 {
        return Err(AuthError::Invalid);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| AuthError::Invalid)?;
    if decoded.len() != 32 || URL_SAFE_NO_PAD.encode(&decoded) != raw {
        return Err(AuthError::Invalid);
    }
    Ok(format!("{:x}", Sha256::digest(token.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_are_unique_hash_only_and_purpose_bound() {
        let purposes = [
            TokenPurpose::Session,
            TokenPurpose::Csrf,
            TokenPurpose::Browser,
            TokenPurpose::OAuth,
            TokenPurpose::Magic,
            TokenPurpose::Invite,
        ];
        for purpose in purposes {
            let token = random_token(purpose).unwrap();
            assert_eq!(digest(purpose, &token).unwrap().len(), 64);
        }

        let first = random_token(TokenPurpose::Magic).unwrap();
        let second = random_token(TokenPurpose::Magic).unwrap();
        assert_ne!(first, second);
        let hashed = digest(TokenPurpose::Magic, &first).unwrap();
        assert_eq!(hashed.len(), 64);
        assert!(hashed
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(!hashed.contains(&first));
        assert!(digest(TokenPurpose::Session, &first).is_err());
    }
    #[test]
    fn malformed_tokens_are_never_hashed_as_credentials() {
        for token in [
            "",
            "old-operator-token",
            "n1.magic.short",
            "n1.magic.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "n1.magic.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
        ] {
            assert!(digest(TokenPurpose::Magic, token).is_err());
        }
    }
}
