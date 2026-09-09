use std::sync::Arc;

use axum::{
    extract::State,
    middleware,
    response::Response,
    routing::{on, post, MethodFilter},
    Router,
};

use super::{handlers, state::TeamState, static_files};

pub(crate) fn build_router(state: Arc<TeamState>) -> Router {
    let authentication = Router::new()
        .route(
            "/auth/github/login",
            on(MethodFilter::GET, handlers::oauth::login)
                .on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route(
            "/auth/github/callback",
            on(MethodFilter::GET, handlers::oauth::callback)
                .on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route(
            "/api/team/onboarding",
            on(MethodFilter::GET, handlers::onboarding::choices)
                .on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route(
            "/api/team/onboarding/select",
            post(handlers::onboarding::select),
        )
        .route(
            "/api/team/session",
            on(MethodFilter::GET, handlers::session::bootstrap)
                .on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route(
            "/api/team/organization",
            on(MethodFilter::GET, handlers::organization::get)
                .on(MethodFilter::PATCH, handlers::organization::patch)
                .on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route("/api/team/logout", post(handlers::session::logout))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            handlers::bounded_auth,
        ));

    Router::new()
        .route(
            "/",
            on(MethodFilter::GET, root).on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route(
            "/api/workos/capabilities",
            on(MethodFilter::GET, capabilities)
                .on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .route(
            "/healthz",
            on(MethodFilter::GET, health).on(MethodFilter::HEAD, handlers::method_not_allowed),
        )
        .merge(authentication)
        .merge(static_files::router(&state.config.static_root))
        .fallback(handlers::not_found)
        .with_state(state)
}

async fn root() -> Response {
    handlers::redirect("/workspace")
}

async fn capabilities(State(_state): State<Arc<TeamState>>) -> Response {
    handlers::json(serde_json::json!({
        "version": 1,
        "mode": "team",
        "runtimeEnabled": false,
    }))
}

async fn health() -> Response {
    handlers::json(serde_json::json!({"status":"ok"}))
}
