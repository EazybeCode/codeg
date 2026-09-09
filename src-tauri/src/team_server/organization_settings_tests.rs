use std::{io::Cursor, sync::Arc};

use axum::http::HeaderMap;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use sea_orm::{
    sea_query::LockType, ActiveModelTrait, DatabaseConnection, EntityTrait, IntoActiveModel,
    QuerySelect, Set, TransactionTrait,
};

use super::{
    organization_settings::{
        read_current, update_current, OrganizationLogoInput, OrganizationPatchInput,
    },
    state::TeamState,
    test_server::team_state,
};
use crate::{
    db::entities::{org, organization_membership, team_session, user, workspace_repository},
    organization_branding::{normalize_logo_data_uri, validate_display_name, BrandingError},
    team_access::{
        principal::ProofRequirement,
        test_auth::{
            cookie_headers, create_postgres_principal_fixture, drop_postgres_fixture,
            principal_fixture, wait_for_postgres_lock, ALICE_A_TOKEN, ALICE_B_TOKEN,
            BOB_A_TOKEN, CAROL_B_TOKEN,
        },
    },
};

fn expect_settings(
    result: Result<super::organization_settings::OrganizationSettingsDto, super::handlers::ApiError>,
) -> super::organization_settings::OrganizationSettingsDto {
    match result {
        Ok(settings) => settings,
        Err(_) => panic!("expected organization settings success"),
    }
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

async fn service_state() -> (TeamState, HeaderMap) {
    let fixture = principal_fixture(Vec::new()).await;
    let principals = fixture.service();
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let state = TeamState::new(
        fixture.db,
        shell.state.config,
        fixture.vault,
        shell.state.oauth,
        shell.state.onboarding,
        fixture.credentials,
        principals,
        fixture.clock,
    );
    (state, cookie_headers(ALICE_A_TOKEN))
}

#[test]
fn organization_settings_name_validation_is_trimmed_unicode_and_bounded() {
    assert_eq!(validate_display_name("  Café ").unwrap(), "Café");
    assert_eq!(validate_display_name(&"界".repeat(80)).unwrap().chars().count(), 80);
    for invalid in ["".to_owned(), "   ".to_owned(), "line\nbreak".to_owned(), "a".repeat(81)] {
        assert_eq!(validate_display_name(&invalid), Err(BrandingError::InvalidName));
    }
}

#[test]
fn organization_settings_logo_validation_rejects_unsupported_and_corrupt_data() {
    for invalid in [
        "https://avatars.example/logo.png",
        "data:image/svg+xml;base64,PHN2Zy8+",
        "data:text/html;base64,PGgxPm5vPC9oMT4=",
        "data:image/png;base64,not-base64***",
        "data:image/jpeg;base64,iVBORw0KGgo=",
    ] {
        assert_eq!(normalize_logo_data_uri(invalid), Err(BrandingError::InvalidLogo));
    }
    let too_large = format!(
        "data:image/png;base64,{}",
        STANDARD.encode(vec![0_u8; 1024 * 1024 + 1])
    );
    assert_eq!(normalize_logo_data_uri(&too_large), Err(BrandingError::InvalidLogo));
    assert_eq!(
        normalize_logo_data_uri(&image_data_uri(ImageFormat::Png, 1025, 1)),
        Err(BrandingError::InvalidLogo)
    );
}

#[test]
fn organization_settings_logo_validation_accepts_and_normalizes_bounded_rasters() {
    for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
        let normalized = normalize_logo_data_uri(&image_data_uri(format, 2, 2)).unwrap();
        assert!(normalized.starts_with("data:image/png;base64,"));
        let decoded = STANDARD.decode(normalized.split_once(',').unwrap().1).unwrap();
        assert_eq!(image::guess_format(&decoded).unwrap(), ImageFormat::Png);
    }
}

#[test]
fn organization_settings_patch_shape_rejects_unknown_authority_and_logo_fields() {
    for body in [
        serde_json::json!({"name":"Atlas","logo":{"action":"keep"},"orgId":202}),
        serde_json::json!({"name":"Atlas","logo":{"action":"keep"},"actor":301}),
        serde_json::json!({"name":"Atlas","logo":{"action":"keep"},"role":"admin"}),
        serde_json::json!({"name":"Atlas","logo":{"action":"keep","data":"ignored"}}),
        serde_json::json!({"name":"Atlas","logo":{"action":"upload"}}),
    ] {
        assert!(serde_json::from_value::<OrganizationPatchInput>(body).is_err());
    }
}

#[tokio::test]
async fn organization_settings_read_uses_verified_scope_and_login_fallback() {
    let (state, headers) = service_state().await;
    let context = state
        .principals
        .resolve(&headers, ProofRequirement::Cached)
        .await
        .unwrap();

    let settings = expect_settings(read_current(&state, &context).await);

    assert_eq!(settings.id, 201);
    assert_eq!(settings.login, "alice-a");
    assert_eq!(settings.name, "alice-a");
    assert!(settings.avatar_url.is_none());
    assert!(settings.can_edit);
}

#[tokio::test]
async fn organization_settings_update_changes_only_selected_branding() {
    let (state, headers) = service_state().await;
    let context = state
        .principals
        .resolve(&headers, ProofRequirement::Cached)
        .await
        .unwrap();
    let before = org::Entity::find_by_id(201).one(&state.db).await.unwrap().unwrap();
    let mapping = workspace_repository::ActiveModel {
        org_id: Set(before.id),
        github_repository_id: Set("9001".into()),
        full_name: Set("alice-a/repository".into()),
        verified_names: Set(r#"["alice-a/repository"]"#.into()),
        created_at: Set(state.clock.now()),
        updated_at: Set(state.clock.now()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .unwrap();

    let settings = expect_settings(update_current(
        &state,
        &context,
        OrganizationPatchInput {
            name: "  Atlas Team  ".into(),
            logo: OrganizationLogoInput::Keep,
        },
    )
    .await);

    assert_eq!(settings.name, "Atlas Team");
    let after = org::Entity::find_by_id(201).one(&state.db).await.unwrap().unwrap();
    assert_eq!(after.github_org_id, before.github_org_id);
    assert_eq!(after.login, before.login);
    assert_eq!(after.avatar_url, before.avatar_url);
    assert!(org::Entity::find_by_id(202).one(&state.db).await.unwrap().unwrap().name.is_none());
    assert_eq!(
        workspace_repository::Entity::find_by_id(mapping.id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .org_id,
        before.id
    );
}

#[tokio::test]
async fn organization_settings_role_and_org_predicates_ignore_legacy_user_tenant_and_role() {
    let (state, _) = service_state().await;
    let alice_a = state
        .principals
        .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    let alice_b = state
        .principals
        .resolve(&cookie_headers(ALICE_B_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    let bob_a = state
        .principals
        .resolve(&cookie_headers(BOB_A_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();
    let carol_b = state
        .principals
        .resolve(&cookie_headers(CAROL_B_TOKEN), ProofRequirement::Cached)
        .await
        .unwrap();

    assert!(expect_settings(read_current(&state, &alice_a).await).can_edit);
    assert!(!expect_settings(read_current(&state, &alice_b).await).can_edit);
    assert!(!expect_settings(read_current(&state, &bob_a).await).can_edit);
    assert!(expect_settings(read_current(&state, &carol_b).await).can_edit);
    assert!(update_current(
        &state,
        &bob_a,
        OrganizationPatchInput {
            name: "Forged Legacy Owner Must Fail".into(),
            logo: OrganizationLogoInput::Keep,
        },
    )
    .await
    .is_err());
    assert!(update_current(
        &state,
        &alice_b,
        OrganizationPatchInput {
            name: "Wrong Scoped Role Must Fail".into(),
            logo: OrganizationLogoInput::Keep,
        },
    )
    .await
    .is_err());
    let changed_b = expect_settings(update_current(
        &state,
        &carol_b,
        OrganizationPatchInput {
            name: "Beta Team".into(),
            logo: OrganizationLogoInput::Keep,
        },
    )
    .await);
    assert_eq!(changed_b.id, 202);
    assert_eq!(changed_b.name, "Beta Team");
    assert_eq!(
        org::Entity::find_by_id(201)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .name,
        None
    );
}

#[tokio::test]
async fn organization_settings_upload_remove_and_keep_use_the_locked_row() {
    let (state, headers) = service_state().await;
    let context = state
        .principals
        .resolve(&headers, ProofRequirement::Cached)
        .await
        .unwrap();
    let uploaded = expect_settings(update_current(
        &state,
        &context,
        OrganizationPatchInput {
            name: "Atlas".into(),
            logo: OrganizationLogoInput::Upload {
                data: image_data_uri(ImageFormat::Jpeg, 2, 2),
            },
        },
    )
    .await);
    assert!(uploaded
        .avatar_url
        .as_deref()
        .is_some_and(|value| value.starts_with("data:image/png;base64,")));
    let kept = expect_settings(update_current(
        &state,
        &context,
        OrganizationPatchInput {
            name: "Atlas Kept".into(),
            logo: OrganizationLogoInput::Keep,
        },
    )
    .await);
    assert_eq!(kept.avatar_url, uploaded.avatar_url);
    let removed = expect_settings(update_current(
        &state,
        &context,
        OrganizationPatchInput {
            name: "Atlas Removed".into(),
            logo: OrganizationLogoInput::Remove,
        },
    )
    .await);
    assert!(removed.avatar_url.is_none());
}

#[tokio::test]
async fn organization_settings_each_final_guard_change_rolls_back_branding() {
    for change in ["identity", "revoked", "generation", "logout"] {
        let (state, headers) = service_state().await;
        let context = state
            .principals
            .resolve(&headers, ProofRequirement::Cached)
            .await
            .unwrap();
        if change == "identity" {
            let person = user::Entity::find_by_id(context.user_id)
                .one(&state.db)
                .await
                .unwrap()
                .unwrap();
            let mut active: user::ActiveModel = person.into();
            active.github_user_id = Set(99_999);
            active.update(&state.db).await.unwrap();
        } else if change == "logout" {
            team_session::Entity::delete_by_id(context.session_id.clone())
                .exec(&state.db)
                .await
                .unwrap();
        } else {
            let membership = organization_membership::Entity::find_by_id(context.membership_id)
                .one(&state.db)
                .await
                .unwrap()
                .unwrap();
            let mut active: organization_membership::ActiveModel = membership.into();
            if change == "revoked" {
                active.state = Set("revoked".into());
                active.authorization_revision = Set(context.authorization_revision + 1);
            } else {
                active.credential_generation = Set(context.credential_generation + 1);
            }
            active.update(&state.db).await.unwrap();
        }

        assert!(update_current(
            &state,
            &context,
            OrganizationPatchInput {
                name: "Must Not Persist".into(),
                logo: OrganizationLogoInput::Remove,
            },
        )
        .await
        .is_err());
        assert!(org::Entity::find_by_id(context.org_id)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap()
            .name
            .is_none());
    }
}

#[tokio::test]
#[ignore = "requires local disposable PostgreSQL"]
async fn organization_settings_postgres_keep_observes_locked_row_and_stale_guard_rolls_back() {
    let (database_name, admin, fixture) = create_postgres_principal_fixture(Vec::new())
        .await
        .expect("create disposable PostgreSQL organization fixture");
    let principals = fixture.service();
    let shell = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let state = Arc::new(TeamState::new(
        fixture.db,
        shell.state.config,
        fixture.vault,
        shell.state.oauth,
        shell.state.onboarding,
        fixture.credentials,
        principals,
        fixture.clock,
    ));
    let result: Result<(), String> = async {
        let context = state
            .principals
            .resolve(&cookie_headers(ALICE_A_TOKEN), ProofRequirement::Cached)
            .await
            .map_err(|_| "resolve PostgreSQL context".to_owned())?;
        let blocker = state.db.begin().await.map_err(|error| error.to_string())?;
        let locked = org::Entity::find_by_id(context.org_id)
            .lock(LockType::Update)
            .one(&blocker)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing PostgreSQL organization".to_owned())?;
        let update_state = Arc::clone(&state);
        let update_context = context.clone();
        let update = tokio::spawn(async move {
            update_current(
                &update_state,
                &update_context,
                OrganizationPatchInput {
                    name: "Postgres Atlas".into(),
                    logo: OrganizationLogoInput::Keep,
                },
            )
            .await
        });
        wait_for_postgres_lock(&admin, &database_name, "FROM \"orgs\"").await?;
        let mut intervening = locked.into_active_model();
        intervening.avatar_url = Set(Some("data:image/png;base64,intervening".into()));
        intervening
            .update(&blocker)
            .await
            .map_err(|error| error.to_string())?;
        blocker.commit().await.map_err(|error| error.to_string())?;
        let updated = tokio::time::timeout(std::time::Duration::from_secs(5), update)
            .await
            .map_err(|_| "timed out waiting for organization update".to_owned())?
            .map_err(|error| error.to_string())?
            .map_err(|_| "organization update rejected".to_owned())?;
        if updated.avatar_url.as_deref() != Some("data:image/png;base64,intervening") {
            return Err("keep did not use the post-lock organization row".into());
        }

        let membership = organization_membership::Entity::find_by_id(context.membership_id)
            .one(&state.db)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing PostgreSQL membership".to_owned())?;
        let mut revoked: organization_membership::ActiveModel = membership.into();
        revoked.state = Set("revoked".into());
        revoked.authorization_revision = Set(context.authorization_revision + 1);
        revoked
            .update(&state.db)
            .await
            .map_err(|error| error.to_string())?;
        if update_current(
            &state,
            &context,
            OrganizationPatchInput {
                name: "Must Roll Back".into(),
                logo: OrganizationLogoInput::Remove,
            },
        )
        .await
        .is_ok()
        {
            return Err("stale PostgreSQL context unexpectedly wrote".into());
        }
        let stored_name = org::Entity::find_by_id(context.org_id)
            .one(&state.db)
            .await
            .map_err(|error| error.to_string())?
            .and_then(|row| row.name);
        if stored_name.as_deref() != Some("Postgres Atlas") {
            return Err("rejected PostgreSQL authority did not roll back".into());
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        panic!("{error}; preserved database {database_name}");
    }
    let db: DatabaseConnection = state.db.clone();
    drop(state);
    drop_postgres_fixture(&database_name, admin, db)
        .await
        .expect("drop verified PostgreSQL organization fixture");
}

#[tokio::test]
async fn organization_settings_stale_context_is_rejected_without_a_write() {
    let (state, headers) = service_state().await;
    let context = state
        .principals
        .resolve(&headers, ProofRequirement::Cached)
        .await
        .unwrap();
    let membership = organization_membership::Entity::find_by_id(context.membership_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap();
    let mut active: organization_membership::ActiveModel = membership.into();
    active.role = Set("member".into());
    active.authorization_revision = Set(context.authorization_revision + 1);
    active.update(&state.db).await.unwrap();

    let result = update_current(
        &state,
        &context,
        OrganizationPatchInput {
            name: "Must Not Persist".into(),
            logo: OrganizationLogoInput::Remove,
        },
    )
    .await;

    assert!(result.is_err());
    assert!(org::Entity::find_by_id(context.org_id)
        .one(&state.db)
        .await
        .unwrap()
        .unwrap()
        .name
        .is_none());
    assert!(team_session::Entity::find_by_id(context.session_id)
        .one(&state.db)
        .await
        .unwrap()
        .is_some());
}
