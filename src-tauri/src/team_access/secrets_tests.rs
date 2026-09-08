use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::secrets::{
    hash_browser_secret, pkce_challenge, random_browser_secret, SecretBinding, SecretError,
    SecretVault,
};

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const OTHER_KEY: &str = "101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f";
const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HASH_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn credential_binding() -> SecretBinding<'static> {
    SecretBinding::GithubCredential {
        user_id: 301,
        github_user_id: 9301,
        generation: 2,
    }
}

fn decode_key(key: &str) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&key[index * 2..index * 2 + 2], 16).unwrap();
    }
    bytes
}

fn external_blob(key: &str, aad: &str, plaintext: &[u8]) -> String {
    let cipher = Aes256Gcm::new_from_slice(&decode_key(key)).unwrap();
    let nonce_bytes = [7_u8; 12];
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .unwrap();
    let mut wire = nonce_bytes.to_vec();
    wire.extend(encrypted);
    format!("v1.{}", STANDARD.encode(wire))
}

#[test]
fn credential_secret_round_trips_only_under_the_exact_identity_binding() {
    let vault = SecretVault::from_hex_key(KEY).unwrap();
    let blob = vault
        .seal(credential_binding(), b"invented-access-token")
        .unwrap();

    assert_eq!(
        vault.open(credential_binding(), &blob).unwrap(),
        b"invented-access-token"
    );
    assert_eq!(
        vault.open(
            SecretBinding::GithubCredential {
                user_id: 302,
                github_user_id: 9301,
                generation: 2,
            },
            &blob,
        ),
        Err(SecretError::Unavailable)
    );
    assert_eq!(
        vault.open(
            SecretBinding::GithubCredential {
                user_id: 301,
                github_user_id: 9302,
                generation: 2,
            },
            &blob,
        ),
        Err(SecretError::Unavailable)
    );
    assert_eq!(
        vault.open(
            SecretBinding::GithubCredential {
                user_id: 301,
                github_user_id: 9301,
                generation: 3,
            },
            &blob,
        ),
        Err(SecretError::Unavailable)
    );
    assert_eq!(
        SecretVault::from_hex_key(OTHER_KEY)
            .unwrap()
            .open(credential_binding(), &blob),
        Err(SecretError::Unavailable)
    );
}

#[test]
fn oauth_and_pending_secrets_bind_every_hash_and_cannot_cross_purposes() {
    let vault = SecretVault::from_hex_key(KEY).unwrap();
    let oauth = SecretBinding::OAuthAttempt {
        state_hash: HASH_A,
        browser_hash: HASH_B,
    };
    let oauth_blob = vault.seal(oauth, b"invented-verifier").unwrap();
    assert_eq!(vault.open(oauth, &oauth_blob).unwrap(), b"invented-verifier");
    assert_eq!(
        vault.open(
            SecretBinding::OAuthAttempt {
                state_hash: HASH_A,
                browser_hash: HASH_C,
            },
            &oauth_blob,
        ),
        Err(SecretError::Unavailable)
    );

    let pending = SecretBinding::PendingOnboarding {
        token_hash: HASH_A,
        github_user_id: 9301,
    };
    let pending_blob = vault.seal(pending, b"invented-pending-token").unwrap();
    assert_eq!(
        vault.open(pending, &pending_blob).unwrap(),
        b"invented-pending-token"
    );
    assert_eq!(
        vault.open(
            SecretBinding::PendingOnboarding {
                token_hash: HASH_B,
                github_user_id: 9301,
            },
            &pending_blob,
        ),
        Err(SecretError::Unavailable)
    );
    assert_eq!(
        vault.open(
            SecretBinding::OAuthAttempt {
                state_hash: HASH_A,
                browser_hash: HASH_B,
            },
            &pending_blob,
        ),
        Err(SecretError::Unavailable)
    );
}

#[test]
fn ciphertext_is_versioned_randomized_and_detects_modification() {
    let vault = SecretVault::from_hex_key(KEY).unwrap();
    let first = vault.seal(credential_binding(), b"invented-token").unwrap();
    let second = vault.seal(credential_binding(), b"invented-token").unwrap();
    assert!(first.starts_with("v1."));
    assert_ne!(first, second);

    let mut payload = STANDARD.decode(first.strip_prefix("v1.").unwrap()).unwrap();
    let last = payload.len() - 1;
    payload[last] ^= 1;
    let modified = format!("v1.{}", STANDARD.encode(payload));
    assert_eq!(
        vault.open(credential_binding(), &modified),
        Err(SecretError::Unavailable)
    );
}

#[test]
fn keys_and_bindings_require_exact_valid_shapes() {
    for invalid_key in [
        "",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f00",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1g",
    ] {
        assert!(matches!(
            SecretVault::from_hex_key(invalid_key),
            Err(SecretError::InvalidInput)
        ));
    }
    let uppercase_key = KEY.to_ascii_uppercase();
    assert!(SecretVault::from_hex_key(&uppercase_key).is_ok());

    let vault = SecretVault::from_hex_key(KEY).unwrap();
    for binding in [
        SecretBinding::OAuthAttempt {
            state_hash: "short",
            browser_hash: HASH_B,
        },
        SecretBinding::OAuthAttempt {
            state_hash: HASH_A,
            browser_hash: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        },
        SecretBinding::PendingOnboarding {
            token_hash: "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
            github_user_id: 9301,
        },
        SecretBinding::PendingOnboarding {
            token_hash: HASH_A,
            github_user_id: 0,
        },
        SecretBinding::GithubCredential {
            user_id: 0,
            github_user_id: 9301,
            generation: 2,
        },
        SecretBinding::GithubCredential {
            user_id: 301,
            github_user_id: -1,
            generation: 2,
        },
        SecretBinding::GithubCredential {
            user_id: 301,
            github_user_id: 9301,
            generation: 0,
        },
    ] {
        assert_eq!(
            vault.seal(binding, b"invented-token"),
            Err(SecretError::InvalidInput)
        );
    }
}

#[test]
fn plaintext_and_encoded_payload_limits_are_enforced_at_the_real_boundary() {
    let vault = SecretVault::from_hex_key(KEY).unwrap();
    assert_eq!(
        vault.seal(credential_binding(), b""),
        Err(SecretError::InvalidInput)
    );
    assert_eq!(
        vault.seal(credential_binding(), &vec![9_u8; 16_385]),
        Err(SecretError::InvalidInput)
    );
    let within_limit = vec![8_u8; 16_384];
    let aad = "workos/team/v1/credential/301/9301/2";
    let accepted = external_blob(KEY, aad, &within_limit);
    assert_eq!(vault.open(credential_binding(), &accepted).unwrap(), within_limit);

    let oversized = external_blob(KEY, aad, &vec![8_u8; 16_385]);
    assert_eq!(
        vault.open(credential_binding(), &oversized),
        Err(SecretError::Unavailable)
    );
    let oversized_encoding = format!("v1.{}", "A".repeat(22_000));
    assert_eq!(
        vault.open(credential_binding(), &oversized_encoding),
        Err(SecretError::Unavailable)
    );
}

#[test]
fn malformed_truncated_and_unknown_ciphertext_is_unavailable_without_secret_details() {
    let vault = SecretVault::from_hex_key(KEY).unwrap();
    for blob in ["v2.AAAA", "v1.%%%", "v1.AAAA", "invented-secret-material"] {
        let error = vault.open(credential_binding(), blob).unwrap_err();
        assert_eq!(error, SecretError::Unavailable);
        assert!(!format!("{error}").contains(blob));
        assert!(!format!("{error:?}").contains(blob));
        assert!(!format!("{error:?}").contains(KEY));
    }
}

#[test]
fn browser_secret_hash_and_pkce_match_independent_sha256_vectors() {
    let verifier = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert_eq!(
        hash_browser_secret(verifier).unwrap(),
        "a8ae6e6ee929abea3afcfc5258c8ccd6f85273e0d4626d26c7279f3250f77c8e"
    );
    assert_eq!(
        pkce_challenge(verifier).unwrap(),
        "qK5ubukpq-o6_PxSWMjM1vhSc-DUYm0mxyefMlD3fI4"
    );
    assert_eq!(pkce_challenge("short"), Err(SecretError::InvalidInput));
    assert_eq!(
        hash_browser_secret("not-a-generated-browser-secret"),
        Err(SecretError::InvalidInput)
    );
}

#[test]
fn generated_browser_secrets_have_valid_shape_and_independent_samples() {
    let first = random_browser_secret().unwrap();
    let second = random_browser_secret().unwrap();
    for secret in [&first, &second] {
        assert_eq!(secret.len(), 64);
        assert!(secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
    }
    assert_ne!(first, second);
}
