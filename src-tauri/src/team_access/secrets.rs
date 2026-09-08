use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};

const MAX_PLAINTEXT_BYTES: usize = 16_384;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_DECODED_BYTES: usize = NONCE_BYTES + TAG_BYTES + MAX_PLAINTEXT_BYTES;
const MAX_ENCODED_BYTES: usize = 4 * MAX_DECODED_BYTES.div_ceil(3);

#[derive(Clone, Copy, Debug)]
pub(crate) enum SecretBinding<'a> {
    OAuthAttempt {
        state_hash: &'a str,
        browser_hash: &'a str,
    },
    PendingOnboarding {
        token_hash: &'a str,
        github_user_id: i64,
    },
    GithubCredential {
        user_id: i32,
        github_user_id: i64,
        generation: i64,
    },
}

pub(crate) struct SecretVault {
    cipher: Aes256Gcm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SecretError {
    #[error("invalid secret input")]
    InvalidInput,
    #[error("secret storage unavailable")]
    Unavailable,
}

impl SecretVault {
    pub(crate) fn from_hex_key(key: &str) -> Result<Self, SecretError> {
        if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(SecretError::InvalidInput);
        }
        let mut bytes = [0_u8; 32];
        for (index, output) in bytes.iter_mut().enumerate() {
            *output = u8::from_str_radix(&key[index * 2..index * 2 + 2], 16)
                .map_err(|_| SecretError::InvalidInput)?;
        }
        let cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|_| SecretError::InvalidInput)?;
        Ok(Self { cipher })
    }

    pub(crate) fn seal(
        &self,
        binding: SecretBinding<'_>,
        plaintext: &[u8],
    ) -> Result<String, SecretError> {
        let aad = binding_aad(binding)?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(SecretError::InvalidInput);
        }
        let mut nonce_bytes = [0_u8; NONCE_BYTES];
        OsRng
            .try_fill_bytes(&mut nonce_bytes)
            .map_err(|_| SecretError::Unavailable)?;
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| SecretError::Unavailable)?;
        let mut wire = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
        wire.extend_from_slice(&nonce_bytes);
        wire.extend_from_slice(&ciphertext);
        Ok(format!("v1.{}", STANDARD.encode(wire)))
    }

    pub(crate) fn open(
        &self,
        binding: SecretBinding<'_>,
        blob: &str,
    ) -> Result<Vec<u8>, SecretError> {
        let aad = binding_aad(binding)?;
        let encoded = blob
            .strip_prefix("v1.")
            .ok_or(SecretError::Unavailable)?;
        if encoded.is_empty() || encoded.len() > MAX_ENCODED_BYTES {
            return Err(SecretError::Unavailable);
        }
        let wire = STANDARD
            .decode(encoded)
            .map_err(|_| SecretError::Unavailable)?;
        if wire.len() < NONCE_BYTES + TAG_BYTES + 1 || wire.len() > MAX_DECODED_BYTES {
            return Err(SecretError::Unavailable);
        }
        let (nonce, ciphertext) = wire.split_at(NONCE_BYTES);
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| SecretError::Unavailable)?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(SecretError::Unavailable);
        }
        Ok(plaintext)
    }
}

pub(crate) fn random_browser_secret() -> Result<String, SecretError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| SecretError::Unavailable)?;
    Ok(lower_hex(&bytes))
}

pub(crate) fn hash_browser_secret(secret: &str) -> Result<String, SecretError> {
    validate_hash(secret)?;
    Ok(lower_hex(&Sha256::digest(secret.as_bytes())))
}

pub(crate) fn pkce_challenge(verifier: &str) -> Result<String, SecretError> {
    validate_hash(verifier)?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())))
}

fn binding_aad(binding: SecretBinding<'_>) -> Result<String, SecretError> {
    let aad = match binding {
        SecretBinding::OAuthAttempt {
            state_hash,
            browser_hash,
        } => {
            validate_hash(state_hash)?;
            validate_hash(browser_hash)?;
            format!("workos/team/v1/oauth/{state_hash}/{browser_hash}")
        }
        SecretBinding::PendingOnboarding {
            token_hash,
            github_user_id,
        } => {
            validate_hash(token_hash)?;
            if github_user_id <= 0 {
                return Err(SecretError::InvalidInput);
            }
            format!("workos/team/v1/pending/{github_user_id}/{token_hash}")
        }
        SecretBinding::GithubCredential {
            user_id,
            github_user_id,
            generation,
        } => {
            if user_id <= 0 || github_user_id <= 0 || generation <= 0 {
                return Err(SecretError::InvalidInput);
            }
            format!("workos/team/v1/credential/{user_id}/{github_user_id}/{generation}")
        }
    };
    Ok(aad)
}

fn validate_hash(value: &str) -> Result<(), SecretError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(SecretError::InvalidInput)
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
