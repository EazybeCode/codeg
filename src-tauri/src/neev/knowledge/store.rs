//! The separate knowledge blob root (`NEEV_KNOWLEDGE_ROOT/{org}/{uuid}.md`).

use super::types::KnowledgeError;
use crate::neev::{content::BlobStore, startup::ServerState};

/// The knowledge store, or `Unavailable` when the root is not configured.
pub(crate) fn store(state: &ServerState) -> Result<&BlobStore, KnowledgeError> {
    state.knowledge.as_deref().ok_or(KnowledgeError::Unavailable)
}

pub(crate) fn new_key() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::test_support as h;

    #[tokio::test]
    async fn unset_root_disables_and_set_root_isolates_orgs() {
        let database = h::database().await;
        let clock = std::sync::Arc::new(h::TestClock::new(1_700_000_000));
        let without =
            ServerState::new(database.clone(), h::preview_config(), clock.clone()).unwrap();
        assert!(matches!(store(&without), Err(KnowledgeError::Unavailable)));
        let with = ServerState::new(database, h::knowledge_config(), clock).unwrap();
        let s = store(&with).unwrap();
        let key = new_key();
        let info = s.put(7, &key, b"# A\n").await.unwrap();
        // Another org's directory never resolves this key.
        assert!(s.read(8, &key, info.byte_length, &info.sha256).await.is_err());
        assert_eq!(
            s.read(7, &key, info.byte_length, &info.sha256).await.unwrap(),
            b"# A\n"
        );
        // A separate root from the document store: the documents blob store
        // cannot read a knowledge blob.
        assert!(with
            .blobs
            .read(7, &key, info.byte_length, &info.sha256)
            .await
            .is_err());
        s.remove_document(7, &key).await.unwrap();
        assert!(s.read(7, &key, info.byte_length, &info.sha256).await.is_err());
        // Removing again is not an error.
        s.remove_document(7, &key).await.unwrap();
    }
}
