use axum::{routing::get_service, Router};
use tower_http::services::{ServeDir, ServeFile};

pub(super) fn router<S>(root: &std::path::Path) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route_service("/login", get_service(ServeFile::new(root.join("login.html"))))
        .route_service(
            "/onboarding",
            get_service(ServeFile::new(root.join("onboarding.html"))),
        )
        .route_service(
            "/workspace",
            get_service(ServeFile::new(root.join("workspace.html"))),
        )
        .route_service(
            "/settings",
            get_service(ServeFile::new(root.join("settings.html"))),
        )
        .route_service(
            "/login.txt",
            get_service(ServeFile::new(root.join("login.txt"))),
        )
        .route_service(
            "/onboarding.txt",
            get_service(ServeFile::new(root.join("onboarding.txt"))),
        )
        .route_service(
            "/workspace.txt",
            get_service(ServeFile::new(root.join("workspace.txt"))),
        )
        .route_service(
            "/settings.txt",
            get_service(ServeFile::new(root.join("settings.txt"))),
        )
        .route_service(
            "/favicon.ico",
            get_service(ServeFile::new(root.join("favicon.ico"))),
        )
        .route_service(
            "/icon-32x32.png",
            get_service(ServeFile::new(root.join("icon-32x32.png"))),
        )
        .nest_service("/_next", ServeDir::new(root.join("_next")))
        .nest_service("/assets", ServeDir::new(root.join("assets")))
        .nest_service("/vs", ServeDir::new(root.join("vs")))
}
