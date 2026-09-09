use std::sync::Arc;

use axum::http::header;
use axum_test::TestServer;

use super::{router::build_router, test_server::team_state};

#[tokio::test]
async fn next_export_route_payloads_are_explicit_without_a_static_catch_all() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let static_root = fixture.state.config.static_root.clone();
    for page in ["login", "onboarding", "workspace", "settings"] {
        std::fs::write(
            static_root.join(format!("{page}.txt")),
            format!("{page}: invented Next route payload"),
        )
        .expect("static route payload");
    }
    std::fs::write(
        static_root.join("operator-secrets.txt"),
        "unported static sentinel",
    )
    .expect("unported sentinel");

    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");
    for page in ["login", "onboarding", "workspace", "settings"] {
        let response = server
            .get(&format!(
                "/{page}.txt?returnChatId=42&remoteConnectionId=777"
            ))
            .await;
        response.assert_status_ok();
        assert_eq!(
            response.text(),
            format!("{page}: invented Next route payload")
        );
        assert_eq!(response.header(header::CONTENT_TYPE), "text/plain");
    }

    std::fs::remove_file(&static_root.join("settings.txt")).expect("remove owned payload");
    server.get("/settings.txt").await.assert_status_not_found();
    let sentinel = server.get("/operator-secrets.txt").await;
    sentinel.assert_status_not_found();
    assert!(!sentinel.text().contains("unported static sentinel"));
}

#[tokio::test]
async fn advertised_icons_are_explicit_without_an_asset_catch_all() {
    let fixture = team_state(Vec::new(), Vec::new(), Vec::new(), Vec::new()).await;
    let static_root = fixture.state.config.static_root.clone();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    let png = b"\x89PNG\r\n\x1a\ninvented-icon";
    std::fs::write(static_root.join("icon.svg"), svg).expect("svg icon");
    std::fs::write(static_root.join("icon-128x128.png"), png).expect("png icon");
    std::fs::write(static_root.join("unported-icon.png"), png).expect("unported icon");

    let server = TestServer::new(build_router(Arc::new(fixture.state))).expect("test server");

    let svg_response = server.get("/icon.svg").await;
    svg_response.assert_status_ok();
    assert_eq!(svg_response.as_bytes().as_ref(), svg);
    assert_eq!(svg_response.header(header::CONTENT_TYPE), "image/svg+xml");

    let png_response = server.get("/icon-128x128.png").await;
    png_response.assert_status_ok();
    assert_eq!(png_response.as_bytes().as_ref(), png);
    assert_eq!(png_response.header(header::CONTENT_TYPE), "image/png");

    std::fs::remove_file(static_root.join("icon.svg")).expect("remove owned icon");
    server.get("/icon.svg").await.assert_status_not_found();
    server
        .get("/unported-icon.png")
        .await
        .assert_status_not_found();
}
