use std::fmt::Write;

use sha2::{Digest, Sha256};

use super::RepositoryDefault;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CreationAudience {
    Private,
    Organization,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintVersion {
    LegacyV1,
    AudienceV2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintError {
    IncompatibleAudience,
}

pub(crate) fn fingerprint(
    version: FingerprintVersion,
    node_id: i32,
    title: Option<&str>,
    repository_override: Option<&RepositoryDefault>,
    creation_audience: CreationAudience,
) -> Result<String, FingerprintError> {
    let legacy_digest = legacy_digest(node_id, title, repository_override);
    let digest = match version {
        FingerprintVersion::LegacyV1 => {
            if creation_audience != CreationAudience::Private {
                return Err(FingerprintError::IncompatibleAudience);
            }
            legacy_digest
        }
        FingerprintVersion::AudienceV2 => {
            let mut hasher = Sha256::new();
            hasher.update(b"neev:chat-create:v2");
            hasher.update([0]);
            hasher.update(legacy_digest);
            hasher.update([match creation_audience {
                CreationAudience::Private => 0,
                CreationAudience::Organization => 1,
            }]);
            hasher.finalize().into()
        }
    };

    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

fn legacy_digest(
    node_id: i32,
    title: Option<&str>,
    repository_override: Option<&RepositoryDefault>,
) -> [u8; 32] {
    fn add_bytes(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    let mut hasher = Sha256::new();
    hasher.update(node_id.to_be_bytes());
    match title {
        Some(title) => {
            hasher.update([1]);
            add_bytes(&mut hasher, title.as_bytes());
        }
        None => hasher.update([0]),
    }
    match repository_override {
        Some(repository_override) => {
            hasher.update([1]);
            hasher.update(repository_override.repository_id.to_be_bytes());
            add_bytes(&mut hasher, repository_override.starting_branch.as_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, CreationAudience, FingerprintError, FingerprintVersion};
    use crate::workspace_catalog::RepositoryDefault;

    #[test]
    fn fixed_inputs_match_independently_computed_version_goldens() {
        let repository = RepositoryDefault {
            repository_id: 29,
            starting_branch: "main".into(),
        };

        assert_eq!(
            fingerprint(
                FingerprintVersion::LegacyV1,
                17,
                Some("Launch plan"),
                Some(&repository),
                CreationAudience::Private,
            )
            .unwrap(),
            "50eae8d13ba7e2089c6a8878a7b6a14b4b3192b9d69800ea9dcbef772dacc1f8"
        );
        assert_eq!(
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("Launch plan"),
                Some(&repository),
                CreationAudience::Private,
            )
            .unwrap(),
            "62dfdd3d12f583c1ae5a00ffb497c649d9df9134f2e3b4a40718c49017491599"
        );
        assert_eq!(
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("Launch plan"),
                Some(&repository),
                CreationAudience::Organization,
            )
            .unwrap(),
            "9bdee5dcea68026e9272d2452c2c1ddb8d7910be9b9b64904eb4b022b823ce2e"
        );
    }

    #[test]
    fn legacy_version_rejects_organization_creation() {
        assert_eq!(
            fingerprint(
                FingerprintVersion::LegacyV1,
                17,
                None,
                None,
                CreationAudience::Organization,
            ),
            Err(FingerprintError::IncompatibleAudience)
        );
    }

    #[test]
    fn unicode_title_uses_utf8_byte_length() {
        assert_eq!(
            fingerprint(
                FingerprintVersion::LegacyV1,
                17,
                Some("文🌱"),
                None,
                CreationAudience::Private,
            )
            .unwrap(),
            "9d5c64de117514a5699ab78a75793ae6b9b0a08cb91ed885c907d50d6ab4710d"
        );
        assert_eq!(
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("文🌱"),
                None,
                CreationAudience::Private,
            )
            .unwrap(),
            "53911b6048fa7d6f4d21532e8996828ec5365512a2fee461b4a8394a09bd7533"
        );
    }

    #[test]
    fn absent_and_empty_titles_have_distinct_stable_encodings() {
        let absent = fingerprint(
            FingerprintVersion::LegacyV1,
            17,
            None,
            None,
            CreationAudience::Private,
        )
        .unwrap();
        let empty = fingerprint(
            FingerprintVersion::LegacyV1,
            17,
            Some(""),
            None,
            CreationAudience::Private,
        )
        .unwrap();

        assert_ne!(absent, empty);
        assert_eq!(
            absent,
            fingerprint(
                FingerprintVersion::LegacyV1,
                17,
                None,
                None,
                CreationAudience::Private,
            )
            .unwrap()
        );
        assert_eq!(
            empty,
            fingerprint(
                FingerprintVersion::LegacyV1,
                17,
                Some(""),
                None,
                CreationAudience::Private,
            )
            .unwrap()
        );
    }

    #[test]
    fn version_two_changes_for_each_creation_input() {
        let repository = RepositoryDefault {
            repository_id: 29,
            starting_branch: "main".into(),
        };
        let baseline = fingerprint(
            FingerprintVersion::AudienceV2,
            17,
            Some("Launch plan"),
            Some(&repository),
            CreationAudience::Private,
        )
        .unwrap();
        let changed_repository = RepositoryDefault {
            repository_id: 30,
            starting_branch: "main".into(),
        };
        let changed_branch = RepositoryDefault {
            repository_id: 29,
            starting_branch: "next".into(),
        };

        for changed in [
            fingerprint(
                FingerprintVersion::AudienceV2,
                18,
                Some("Launch plan"),
                Some(&repository),
                CreationAudience::Private,
            )
            .unwrap(),
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("Revised plan"),
                Some(&repository),
                CreationAudience::Private,
            )
            .unwrap(),
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("Launch plan"),
                Some(&changed_repository),
                CreationAudience::Private,
            )
            .unwrap(),
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("Launch plan"),
                Some(&changed_branch),
                CreationAudience::Private,
            )
            .unwrap(),
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                Some("Launch plan"),
                Some(&repository),
                CreationAudience::Organization,
            )
            .unwrap(),
        ] {
            assert_ne!(changed, baseline);
        }
    }

    #[test]
    fn later_visibility_changes_do_not_replace_original_creation_audience() {
        let creation_audience = CreationAudience::Organization;
        let before_share = fingerprint(
            FingerprintVersion::AudienceV2,
            17,
            None,
            None,
            creation_audience,
        )
        .unwrap();
        let _current_audience = CreationAudience::Private;

        assert_eq!(
            fingerprint(
                FingerprintVersion::AudienceV2,
                17,
                None,
                None,
                creation_audience,
            )
            .unwrap(),
            before_share
        );
    }
}
