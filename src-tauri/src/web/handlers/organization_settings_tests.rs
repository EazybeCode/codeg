use std::io::Cursor;

use axum::http::HeaderMap;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use super::{
    organization_settings::{get_core, patch_core, SettingsError},
    workspace_catalog::BrowserMutationPolicy,
};
use crate::db::{
    entities::{org, session, user, workspace_repository},
    test_helpers::fresh_in_memory_db,
};

async fn seed_identity(
    db: &sea_orm::DatabaseConnection,
    suffix: &str,
    role: &str,
) -> (org::Model, user::Model, HeaderMap) {
    let now = Utc::now();
    let organization = org::ActiveModel {
        github_org_id: Set(if suffix == "a" { 101 } else { 202 }),
        login: Set(format!("github-{suffix}")),
        name: Set(Some(format!("Organization {suffix}"))),
        avatar_url: Set(Some(format!(
            "data:image/png;base64,existing-{suffix}"
        ))),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    let person = user::ActiveModel {
        github_user_id: Set(if suffix == "a" { 301 } else { 402 }),
        login: Set(format!("user-{suffix}")),
        org_id: Set(organization.id),
        role: Set(role.into()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
    let sid = format!("session-{suffix}-{role}");
    session::ActiveModel {
        id: Set(sid.clone()),
        user_id: Set(person.id),
        created_at: Set(now),
        expires_at: Set(now + chrono::Duration::hours(1)),
    }
    .insert(db)
    .await
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("cookie", format!("workos_session={sid}").parse().unwrap());
    (organization, person, headers)
}

fn app_headers(mut headers: HeaderMap) -> HeaderMap {
    headers.insert("origin", "https://workos.example".parse().unwrap());
    headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
    headers
}

fn image_data_uri(format: ImageFormat, width: u32, height: u32) -> String {
    let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
        width,
        height,
        Rgba([12, 34, 56, 255]),
    ));
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
    let media = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => unreachable!(),
    };
    format!("data:{media};base64,{}", STANDARD.encode(bytes))
}

#[tokio::test]
async fn get_uses_the_cookie_session_and_returns_only_its_organization() {
    let db = fresh_in_memory_db().await;
    let (first, _, first_headers) = seed_identity(&db.conn, "a", "member").await;
    let (second, _, _) = seed_identity(&db.conn, "b", "owner").await;

    let settings = get_core(&first_headers, &db.conn).await.unwrap();

    assert_eq!(settings.id, first.id);
    assert_eq!(settings.login, "github-a");
    assert_eq!(settings.name, "Organization a");
    assert_eq!(
        settings.avatar_url.as_deref(),
        Some("data:image/png;base64,existing-a")
    );
    assert!(!settings.can_edit);
    assert_ne!(settings.id, second.id);
    assert!(matches!(
        get_core(&HeaderMap::new(), &db.conn).await,
        Err(SettingsError::Unauthorized)
    ));
}

#[tokio::test]
async fn members_cannot_patch_organization_settings() {
    let db = fresh_in_memory_db().await;
    let (organization, _, headers) = seed_identity(&db.conn, "a", "member").await;

    let result = patch_core(
        &app_headers(headers),
        &db.conn,
        &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
        br#"{"name":"Changed"}"#,
    )
    .await;

    assert!(matches!(result, Err(SettingsError::Forbidden)));
    assert_eq!(
        org::Entity::find_by_id(organization.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("Organization a")
    );
}

#[tokio::test]
async fn owners_and_admins_update_only_their_current_org_without_touching_github_identity() {
    for role in ["owner", "admin"] {
        let db = fresh_in_memory_db().await;
        let (first, _, headers) = seed_identity(&db.conn, "a", role).await;
        let (second, _, _) = seed_identity(&db.conn, "b", "owner").await;
        let now = Utc::now();
        let mapping = workspace_repository::ActiveModel {
            org_id: Set(first.id),
            github_repository_id: Set("9001".into()),
            full_name: Set("github-a/repository".into()),
            verified_names: Set(r#"["github-a/repository"]"#.into()),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let updated = patch_core(
            &app_headers(headers),
            &db.conn,
            &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
            br#"{"name":"  New Display Name  ","logo":{"action":"keep"}}"#,
        )
        .await
        .unwrap();

        assert_eq!(updated.name, "New Display Name");
        let stored = org::Entity::find_by_id(first.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.github_org_id, 101);
        assert_eq!(stored.login, "github-a");
        assert_eq!(stored.avatar_url, first.avatar_url);
        assert_eq!(
            org::Entity::find_by_id(second.id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("Organization b")
        );
        assert_eq!(
            workspace_repository::Entity::find_by_id(mapping.id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap()
                .org_id,
            first.id
        );
    }
}

#[tokio::test]
async fn patch_rejects_cross_site_browser_requests_before_mutating() {
    let db = fresh_in_memory_db().await;
    let (organization, _, mut headers) = seed_identity(&db.conn, "a", "owner").await;
    headers.insert("origin", "https://evil.example".parse().unwrap());

    let result = patch_core(
        &headers,
        &db.conn,
        &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
        br#"{"name":"Changed"}"#,
    )
    .await;

    assert!(matches!(result, Err(SettingsError::BadRequest(_))));
    assert_eq!(
        org::Entity::find_by_id(organization.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("Organization a")
    );
}

#[tokio::test]
async fn patch_strictly_validates_json_and_display_name() {
    let invalid_bodies: [&[u8]; 6] = [
        br#"{"name":""}"#,
        br#"{"name":"   "}"#,
        br#"{"name":"line\nbreak"}"#,
        br#"{"name":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        br#"{"name":"Acme","orgId":42}"#,
        br#"{"name":"Acme","logo":{"action":"delete"}}"#,
    ];
    for body in invalid_bodies {
        let db = fresh_in_memory_db().await;
        let (_, _, headers) = seed_identity(&db.conn, "a", "owner").await;
        let result = patch_core(
            &app_headers(headers),
            &db.conn,
            &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
            body,
        )
        .await;
        assert!(
            matches!(result, Err(SettingsError::BadRequest(_))),
            "body: {}",
            String::from_utf8_lossy(body)
        );
    }
}

#[tokio::test]
async fn accepted_logo_formats_are_reencoded_as_png_and_can_be_removed() {
    for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
        let db = fresh_in_memory_db().await;
        let (_, _, headers) = seed_identity(&db.conn, "a", "owner").await;
        let body = serde_json::json!({
            "name": "Organization a",
            "logo": { "action": "upload", "data": image_data_uri(format, 2, 2) }
        });
        let updated = patch_core(
            &app_headers(headers.clone()),
            &db.conn,
            &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
            &serde_json::to_vec(&body).unwrap(),
        )
        .await
        .unwrap();
        let encoded = updated.avatar_url.unwrap();
        assert!(encoded.starts_with("data:image/png;base64,"));
        let png = STANDARD.decode(encoded.split_once(',').unwrap().1).unwrap();
        assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);

        let removed = patch_core(
            &app_headers(headers),
            &db.conn,
            &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
            br#"{"name":"Organization a","logo":{"action":"remove"}}"#,
        )
        .await
        .unwrap();
        assert!(removed.avatar_url.is_none());
    }
}

#[tokio::test]
async fn logo_validation_rejects_svg_malformed_oversized_and_excessive_dimensions() {
    let cases = [
        "data:image/svg+xml;base64,PHN2Zy8+".to_string(),
        "data:image/png;base64,not-base64***".to_string(),
        format!(
            "data:image/png;base64,{}",
            STANDARD.encode(vec![0_u8; 1024 * 1024 + 1])
        ),
        image_data_uri(ImageFormat::Png, 1025, 1),
    ];
    for data in cases {
        let db = fresh_in_memory_db().await;
        let (_, _, headers) = seed_identity(&db.conn, "a", "owner").await;
        let body = serde_json::json!({
            "name": "Organization a",
            "logo": { "action": "upload", "data": data }
        });
        let result = patch_core(
            &app_headers(headers),
            &db.conn,
            &BrowserMutationPolicy::from_app_origin(Some("https://workos.example")),
            &serde_json::to_vec(&body).unwrap(),
        )
        .await;
        assert!(matches!(result, Err(SettingsError::BadRequest(_))));
    }
}
