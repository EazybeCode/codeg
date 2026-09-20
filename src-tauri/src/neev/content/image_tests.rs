use super::{
    image, resources,
    rich::{RichBlock, RichDocument},
    test_support::{patch, Harness},
    types::*,
};
use crate::neev::test_support as h;

// A blob whose leading bytes sniff as PNG. This exercises storage + ACL, not the
// image decoder, so a minimal valid signature is sufficient.
fn png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0u8; 24]);
    bytes
}

async fn document(harness: &Harness) -> Resource {
    resources::create(
        &harness.state,
        &harness.creator,
        harness.input(ResourceKind::Document, "Private doc"),
    )
    .await
    .unwrap()
}

fn image_block(id: &str, key: &str) -> RichDocument {
    RichDocument {
        version: 1,
        blocks: vec![RichBlock::Image {
            id: id.into(),
            children: vec![],
            url: format!("/api/neev/images/{key}"),
            caption: String::new(),
        }],
    }
}

#[tokio::test]
async fn image_serve_enforces_the_bound_documents_private_acl() {
    let harness = Harness::new(h::database().await).await;
    let doc = document(&harness).await;
    assert_eq!(doc.visibility, Visibility::Private);
    let stored = image::store(&harness.state, &harness.creator, &doc.id, &png())
        .await
        .unwrap();
    assert_eq!(stored.content_type, "image/png");
    // The creator (authorized for the private document) can read the image.
    let (bytes, content_type) = image::load(&harness.state, &harness.creator, &stored.key)
        .await
        .unwrap();
    assert_eq!(content_type, "image/png");
    assert_eq!(bytes, png());
    // A same-org member who is NOT authorized for the private document cannot read
    // it, even knowing the key; nor can a member of another organization.
    assert_eq!(
        image::load(&harness.state, &harness.admin, &stored.key)
            .await
            .err(),
        Some(ContentError::NotFound)
    );
    assert_eq!(
        image::load(&harness.state, &harness.foreign, &stored.key)
            .await
            .err(),
        Some(ContentError::NotFound)
    );
}

#[tokio::test]
async fn organization_document_image_follows_the_document_audience() {
    let harness = Harness::new(h::database().await).await;
    let doc = document(&harness).await;
    let mut p = patch(&doc);
    p.visibility = Visibility::Organization;
    let doc = resources::update(&harness.state, &harness.creator, &doc.id, p)
        .await
        .unwrap();
    let stored = image::store(&harness.state, &harness.creator, &doc.id, &png())
        .await
        .unwrap();
    // A same-org member can read an org-visible document's image; a foreign org cannot.
    assert!(image::load(&harness.state, &harness.admin, &stored.key)
        .await
        .is_ok());
    assert_eq!(
        image::load(&harness.state, &harness.foreign, &stored.key)
            .await
            .err(),
        Some(ContentError::NotFound)
    );
}

#[tokio::test]
async fn save_refuses_an_image_bound_to_another_document_or_unknown() {
    let harness = Harness::new(h::database().await).await;
    let doc_a = document(&harness).await;
    let doc_b = resources::create(
        &harness.state,
        &harness.creator,
        harness.input(ResourceKind::Document, "Doc B"),
    )
    .await
    .unwrap();
    let stored = image::store(&harness.state, &harness.creator, &doc_a.id, &png())
        .await
        .unwrap();
    let block_id = "11111111-1111-4111-8111-111111111111";
    // Embedding A's image in A is fine; embedding it in B widens its audience and
    // is refused.
    assert!(image::verify_document_images(
        &harness.state,
        &harness.creator,
        &doc_a.id,
        &image_block(block_id, &stored.key)
    )
    .await
    .is_ok());
    assert_eq!(
        image::verify_document_images(
            &harness.state,
            &harness.creator,
            &doc_b.id,
            &image_block(block_id, &stored.key)
        )
        .await
        .err(),
        Some(ContentError::Invalid)
    );
    // An unknown key is refused too.
    assert_eq!(
        image::verify_document_images(
            &harness.state,
            &harness.creator,
            &doc_a.id,
            &image_block(block_id, "00000000-0000-4000-8000-000000000000")
        )
        .await
        .err(),
        Some(ContentError::Invalid)
    );
}

#[tokio::test]
async fn backup_verification_includes_and_checks_document_images() {
    let harness = Harness::new(h::database().await).await;
    let doc = document(&harness).await;
    let stored = image::store(&harness.state, &harness.creator, &doc.id, &png())
        .await
        .unwrap();
    // The snapshot verifier reaches the image blob, confirms its bytes/sha, and
    // accounts for it in the manifest (backup/restore completeness).
    let manifest =
        super::backup::verify_snapshot(&harness.state.database, &harness.state.blobs)
            .await
            .unwrap();
    assert_eq!(manifest.schema_version, 11);
    assert!(manifest.blobs.iter().any(|blob| blob.key == stored.key));
}

#[tokio::test]
async fn store_refuses_non_images_and_unauthorized_documents() {
    let harness = Harness::new(h::database().await).await;
    let doc = document(&harness).await;
    // Not image bytes.
    assert_eq!(
        image::store(&harness.state, &harness.creator, &doc.id, b"not an image")
            .await
            .err(),
        Some(ContentError::Invalid)
    );
    // A foreign-org member cannot upload to this org's document.
    assert_eq!(
        image::store(&harness.state, &harness.foreign, &doc.id, &png())
            .await
            .err(),
        Some(ContentError::NotFound)
    );
    // A same-org non-creator cannot upload to a PRIVATE document.
    assert_eq!(
        image::store(&harness.state, &harness.admin, &doc.id, &png())
            .await
            .err(),
        Some(ContentError::NotFound)
    );
}
