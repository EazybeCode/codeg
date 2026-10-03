use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use async_trait::async_trait;

use super::*;
use crate::neev::agents::capabilities::{provider_capabilities, ModelDto};

struct Fake {
    answer: Mutex<Option<Result<Decision, DecisionError>>>,
    calls: AtomicUsize,
    seen: Mutex<Option<DecisionInput>>,
}

impl Fake {
    fn new(answer: Result<Decision, DecisionError>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Some(answer)),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(None),
        })
    }
}

#[async_trait]
impl DecisionProvider for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn decide(&self, input: &DecisionInput) -> Result<Decision, DecisionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.seen.lock().unwrap() = Some(input.clone());
        self.answer.lock().unwrap().take().unwrap()
    }
}

fn ready(provider: Arc<Fake>) -> Router {
    Router::Ready {
        provider,
        min_confidence: 0.3,
    }
}

fn model(id: &str, label: &str, efforts: Option<&[&str]>) -> ModelDto {
    ModelDto {
        id: id.into(),
        label: label.into(),
        default: id == "default",
        supports_speed: false,
        efforts: efforts.map(|list| list.iter().map(|e| (*e).to_owned()).collect()),
        default_effort: None,
    }
}

fn codex_catalogue() -> crate::neev::agents::capabilities::ProviderCapabilities {
    let mut codex = provider_capabilities("codex");
    codex.models = vec![
        model("gpt-a", "GPT A", Some(&["low", "medium", "high"])),
        model("gpt-b", "GPT B", Some(&[])),
    ];
    codex
}

fn manual() -> Selection {
    Selection {
        provider: "claude".into(),
        model: "sonnet".into(),
        effort: Some("medium".into()),
        fast: true,
    }
}

fn ask() -> Ask<'static> {
    Ask {
        prompt: "Refactor the billing module and add tests",
        attached_files: 1,
        earlier_turns: 2,
        previous: None,
    }
}

fn claude_candidates() -> Vec<Candidate> {
    candidates(&[provider_capabilities("claude")], &["claude"])
}

#[test]
fn candidates_come_only_from_connected_providers() {
    let list = candidates(
        &[provider_capabilities("claude"), codex_catalogue()],
        &["codex"],
    );
    assert!(!list.is_empty());
    assert!(list.iter().all(|entry| entry.provider == "codex"));
    assert_eq!(
        list.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(),
        ["gpt-a", "gpt-b"]
    );
}

#[test]
fn candidates_skip_the_unnamed_default_when_real_models_exist() {
    let list = claude_candidates();
    assert!(list.iter().all(|entry| entry.model != "default"));
    assert!(list.iter().any(|entry| entry.model == "opus"));
}

#[test]
fn a_provider_with_only_its_default_still_offers_it() {
    let list = candidates(&[provider_capabilities("codex")], &["codex"]);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].model, "default");
}

#[test]
fn a_model_that_names_its_efforts_overrides_the_provider_scale() {
    let list = candidates(&[codex_catalogue()], &["codex"]);
    assert_eq!(list[0].efforts, ["low", "medium", "high"]);
    assert!(list[1].efforts.is_empty());
    let claude = claude_candidates();
    assert_eq!(claude[0].efforts, ["low", "medium", "high", "xhigh", "max"]);
}

#[test]
fn candidates_are_bounded() {
    let many = |provider: &'static str| {
        let mut catalogue = provider_capabilities(provider);
        catalogue.models = (0..100)
            .map(|n| model(&format!("m{n}"), &format!("M{n}"), None))
            .collect();
        catalogue
    };
    assert_eq!(
        candidates(&[many("codex")], &["codex"]).len(),
        MAX_PER_PROVIDER
    );
    assert_eq!(
        candidates(
            &[many("claude"), many("codex"), many("cursor")],
            &["claude", "codex", "cursor"]
        )
        .len(),
        MAX_CANDIDATES
    );
}

#[test]
fn effort_maps_complexity_onto_only_the_allowed_levels() {
    let all: Vec<String> = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    // `max` is never chosen automatically: it is the most expensive level and
    // an explicit, manual choice.
    assert_eq!(effort_for(&all, 0.0, false).as_deref(), Some("low"));
    assert_eq!(effort_for(&all, 1.0, false).as_deref(), Some("medium"));
    assert_eq!(effort_for(&all, 2.0, false).as_deref(), Some("high"));
    assert_eq!(effort_for(&all, 3.0, false).as_deref(), Some("xhigh"));
    let narrow = vec!["minimal".to_owned(), "low".to_owned()];
    assert_eq!(effort_for(&narrow, 3.0, false).as_deref(), Some("low"));
    let odd = vec!["turbo".to_owned()];
    assert_eq!(effort_for(&odd, 2.0, false).as_deref(), Some("turbo"));
    assert_eq!(effort_for(&[], 2.0, false), None);
}

#[test]
fn the_task_sent_to_the_router_is_bounded_on_a_char_boundary() {
    let long = "é".repeat(MAX_TASK_CHARS + 50);
    let bounded = bound_task(&long);
    assert!(bounded.chars().count() <= MAX_TASK_CHARS + 32);
    assert!(bounded.ends_with("[truncated]"));
    assert_eq!(bound_task("short"), "short");
}

#[tokio::test]
async fn off_never_calls_anything_and_keeps_the_manual_selection() {
    let route = Router::Off
        .route(ask(), &claude_candidates(), manual())
        .await;
    assert_eq!(route.selection, manual());
    assert_eq!(
        route.outcome,
        Outcome::Fallback(FallbackReason::NotConfigured)
    );
    assert_eq!(Router::Off.status(), RouterStatus::Off);
}

#[tokio::test]
async fn a_missing_credential_is_visible_and_never_reported_ready() {
    let router = Router::MissingCredential { router: "jev" };
    assert_eq!(router.status(), RouterStatus::MissingCredential);
    assert_eq!(router.status().code(), "missing_credential");
    let route = router.route(ask(), &claude_candidates(), manual()).await;
    assert_eq!(route.selection, manual());
    assert_eq!(
        route.outcome,
        Outcome::Fallback(FallbackReason::MissingCredential)
    );
    assert_eq!(route.router, None);
}

#[tokio::test]
async fn a_confident_decision_routes_to_that_allowed_candidate() {
    let list = claude_candidates();
    let opus = list.iter().position(|c| c.model == "opus").unwrap();
    let fake = Fake::new(Ok(Decision {
        option: opus,
        confidence: 0.8,
        complexity: 2.0,
        complexity_confidence: 0.9,
    }));
    let route = ready(fake.clone()).route(ask(), &list, manual()).await;
    assert_eq!(
        route.selection,
        Selection {
            provider: "claude".into(),
            model: "opus".into(),
            effort: Some("high".into()),
            // Auto never turns on a faster, costlier tier by itself.
            fast: false,
        }
    );
    assert_eq!(route.outcome, Outcome::Routed);
    assert_eq!(route.router, Some("fake"));
    assert_eq!(route.label.as_deref(), Some("Opus"));
    let seen = fake.seen.lock().unwrap().clone().unwrap();
    assert_eq!(seen.options.len(), list.len());
    assert_eq!(seen.attached_files, 1);
    assert_eq!(seen.earlier_turns, 2);
}

#[tokio::test]
async fn a_model_with_no_effort_control_routes_without_one() {
    let list = candidates(&[codex_catalogue()], &["codex"]);
    let fake = Fake::new(Ok(Decision {
        option: 1,
        confidence: 0.9,
        complexity: 3.0,
        complexity_confidence: 0.9,
    }));
    let route = ready(fake).route(ask(), &list, manual()).await;
    assert_eq!(route.selection.model, "gpt-b");
    assert_eq!(route.selection.effort, None);
}

#[tokio::test]
async fn an_answer_outside_the_allowlist_falls_back() {
    let list = claude_candidates();
    let fake = Fake::new(Ok(Decision {
        option: list.len(),
        confidence: 0.99,
        complexity: 1.0,
        complexity_confidence: 0.9,
    }));
    let route = ready(fake).route(ask(), &list, manual()).await;
    assert_eq!(route.selection, manual());
    assert_eq!(route.outcome, Outcome::Fallback(FallbackReason::Malformed));
    assert_eq!(route.router, Some("fake"));
}

#[tokio::test]
async fn non_finite_or_out_of_range_numbers_fall_back() {
    for (confidence, complexity) in [
        (f64::NAN, 1.0),
        (0.9, f64::INFINITY),
        (1.5, 1.0),
        (0.9, -0.5),
        (0.9, 3.5),
    ] {
        let fake = Fake::new(Ok(Decision {
            option: 0,
            confidence,
            complexity,
            complexity_confidence: 0.9,
        }));
        let route = ready(fake)
            .route(ask(), &claude_candidates(), manual())
            .await;
        assert_eq!(
            route.outcome,
            Outcome::Fallback(FallbackReason::Malformed),
            "{confidence} {complexity}"
        );
    }
}

#[tokio::test]
async fn low_confidence_falls_back_to_the_manual_selection() {
    let fake = Fake::new(Ok(Decision {
        option: 0,
        confidence: 0.1,
        complexity: 1.0,
        complexity_confidence: 0.9,
    }));
    let route = ready(fake)
        .route(ask(), &claude_candidates(), manual())
        .await;
    assert_eq!(route.selection, manual());
    assert_eq!(
        route.outcome,
        Outcome::Fallback(FallbackReason::LowConfidence)
    );
}

#[tokio::test]
async fn provider_failures_fall_back_with_their_reason() {
    for (error, reason) in [
        (DecisionError::Timeout, FallbackReason::Timeout),
        (DecisionError::Unavailable, FallbackReason::Unavailable),
        (DecisionError::Malformed, FallbackReason::Malformed),
    ] {
        let route = ready(Fake::new(Err(error)))
            .route(ask(), &claude_candidates(), manual())
            .await;
        assert_eq!(route.selection, manual());
        assert_eq!(route.outcome, Outcome::Fallback(reason));
    }
}

#[tokio::test]
async fn no_candidates_means_no_router_call() {
    let fake = Fake::new(Err(DecisionError::Unavailable));
    let route = ready(fake.clone()).route(ask(), &[], manual()).await;
    assert_eq!(
        route.outcome,
        Outcome::Fallback(FallbackReason::NoCandidates)
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_router_that_never_answers_is_cut_off_by_the_deadline() {
    struct Hang;
    #[async_trait]
    impl DecisionProvider for Hang {
        fn name(&self) -> &'static str {
            "hang"
        }
        async fn decide(&self, _: &DecisionInput) -> Result<Decision, DecisionError> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Err(DecisionError::Unavailable)
        }
    }
    let router = Router::Ready {
        provider: Arc::new(Hang),
        min_confidence: 0.3,
    };
    let started = std::time::Instant::now();
    let route = router
        .route_within(
            ask(),
            &claude_candidates(),
            manual(),
            Duration::from_millis(50),
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(route.outcome, Outcome::Fallback(FallbackReason::Timeout));
}

// ---- Jev over HTTP, against a loopback stand-in ----

mod jev_http {
    use super::*;
    use axum::{extract::State, http::HeaderMap, routing::post, Json};
    use tokio::sync::Mutex as AsyncMutex;

    type Captured = Arc<AsyncMutex<Option<(HeaderMap, serde_json::Value)>>>;

    async fn serve(
        status: u16,
        body: String,
        delay: Duration,
    ) -> (url::Url, Captured, tokio::task::JoinHandle<()>) {
        let captured: Captured = Arc::new(AsyncMutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route(
                "/v1/systemone",
                post(
                    move |State(captured): State<Captured>,
                          headers: HeaderMap,
                          Json(request): Json<serde_json::Value>| {
                        let body = body.clone();
                        async move {
                            *captured.lock().await = Some((headers, request));
                            tokio::time::sleep(delay).await;
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                [("content-type", "application/json")],
                                body,
                            )
                        }
                    },
                ),
            )
            .with_state(captured.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            url::Url::parse(&format!("http://{address}/v1/systemone")).unwrap(),
            captured,
            server,
        )
    }

    fn input() -> DecisionInput {
        DecisionInput {
            task: "Fix the flaky test".into(),
            attached_files: 0,
            earlier_turns: 3,
            previous: Some(("Plan the migration".into(), "Here is the plan".into())),
            options: vec![
                RouteOption {
                    agent: "Claude Code".into(),
                    model: "Haiku".into(),
                    remaining_usage: "plenty",
                },
                RouteOption {
                    agent: "Claude Code".into(),
                    model: "Opus".into(),
                    remaining_usage: "unknown",
                },
            ],
        }
    }

    fn answer(choice: &str, confidence: f64, score: f64) -> String {
        serde_json::json!({
            "model": "jev-1.13.0",
            "answers": {
                "route": {
                    "type": "choice",
                    "choice": choice,
                    "probabilities": {"o1": 0.1, "o2": 0.9},
                    "confidence": confidence
                },
                "complexity": {
                    "type": "score",
                    "score": score,
                    "legend": {"0": "a", "1": "b", "2": "c", "3": "d"},
                    "probabilities": {"0": 0.0, "1": 0.2, "2": 0.8, "3": 0.0},
                    "confidence": 0.7
                }
            },
            "usage": {"input_tokens": 300, "output_tokens": 30}
        })
        .to_string()
    }

    fn jev(endpoint: url::Url, timeout: Duration) -> Jev {
        Jev::for_test(
            "test-router-key".into(),
            JevModel::Pinned,
            timeout,
            endpoint,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn sends_the_documented_request_and_reads_a_valid_answer() {
        let (url, captured, server) = serve(200, answer("o2", 0.8, 1.8), Duration::ZERO).await;
        let decision = jev(url, Duration::from_secs(2))
            .decide(&input())
            .await
            .unwrap();
        assert_eq!(decision.option, 1);
        assert!((decision.confidence - 0.8).abs() < 1e-9);
        assert!((decision.complexity - 1.8).abs() < 1e-9);
        let (headers, body) = captured.lock().await.take().unwrap();
        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer test-router-key"
        );
        assert_eq!(body["model"], "jev-1.13.0");
        assert_eq!(body["state"]["task"], "Fix the flaky test");
        assert_eq!(body["state"]["earlier_turns"], 3);
        assert_eq!(body["questions"]["route"]["type"], "choice");
        assert_eq!(
            body["questions"]["route"]["criteria"],
            serde_json::json!({
                "o1": {"agent": "Claude Code", "model": "Haiku", "remaining_usage": "plenty"},
                "o2": {"agent": "Claude Code", "model": "Opus", "remaining_usage": "unknown"}
            })
        );
        assert_eq!(
            body["state"]["previous"],
            serde_json::json!({"request": "Plan the migration", "reply": "Here is the plan"})
        );
        // No cost or capability is asserted about any option.
        let instructions = body["questions"]["route"]["instructions"].as_str().unwrap();
        assert!(!instructions.contains("cost"));
        // Usage is a word per option, explained once; never a number.
        assert!(instructions.contains("`remaining_usage`"));
        assert!(!body.to_string().contains("used_percent"));
        assert_eq!(body["questions"]["complexity"]["type"], "score");
        assert_eq!(
            body["questions"]["complexity"]["criteria"]
                .as_array()
                .unwrap()
                .len(),
            COMPLEXITY_LEVELS.len()
        );
        server.abort();
    }

    #[tokio::test]
    async fn an_option_it_was_not_offered_is_malformed() {
        let (url, _, server) = serve(200, answer("o9", 0.9, 1.0), Duration::ZERO).await;
        assert_eq!(
            jev(url, Duration::from_secs(2))
                .decide(&input())
                .await
                .unwrap_err(),
            DecisionError::Malformed
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_body_that_is_not_the_contract_is_malformed() {
        for body in [
            "not json".to_owned(),
            "{}".to_owned(),
            answer("o1", 0.9, 1.0).replace("\"type\":\"choice\"", "\"type\":\"score\""),
        ] {
            let (url, _, server) = serve(200, body, Duration::ZERO).await;
            assert_eq!(
                jev(url, Duration::from_secs(2))
                    .decide(&input())
                    .await
                    .unwrap_err(),
                DecisionError::Malformed
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_RESPONSE_BYTES + 10));
        let (url, _, server) = serve(200, big, Duration::ZERO).await;
        assert_eq!(
            jev(url, Duration::from_secs(2))
                .decide(&input())
                .await
                .unwrap_err(),
            DecisionError::Malformed
        );
        server.abort();
    }

    #[tokio::test]
    async fn overload_is_unavailable_but_a_refused_request_is_rejected() {
        for (status, expected) in [
            (429, DecisionError::Unavailable),
            (500, DecisionError::Unavailable),
            (529, DecisionError::Unavailable),
            (400, DecisionError::Rejected),
            (401, DecisionError::Rejected),
            (403, DecisionError::Rejected),
            (422, DecisionError::Rejected),
        ] {
            let (url, _, server) = serve(status, "{}".into(), Duration::ZERO).await;
            assert_eq!(
                jev(url, Duration::from_secs(2))
                    .decide(&input())
                    .await
                    .unwrap_err(),
                expected,
                "{status}"
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed_so_the_key_goes_nowhere_else() {
        let (url, _, server) = serve(307, "{}".into(), Duration::ZERO).await;
        assert_eq!(
            jev(url, Duration::from_secs(2))
                .decide(&input())
                .await
                .unwrap_err(),
            DecisionError::Unavailable
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_slow_answer_is_a_timeout() {
        let (url, _, server) = serve(200, answer("o1", 0.9, 1.0), Duration::from_secs(5)).await;
        assert_eq!(
            jev(url, Duration::from_millis(100))
                .decide(&input())
                .await
                .unwrap_err(),
            DecisionError::Timeout
        );
        server.abort();
    }

    #[test]
    fn production_constructor_refuses_a_bad_key_and_test_constructor_requires_loopback() {
        assert!(Jev::new("".into(), JevModel::Pinned, Duration::from_secs(1)).is_err());
        assert!(Jev::new("bad\nkey".into(), JevModel::Pinned, Duration::from_secs(1)).is_err());
        assert!(Jev::new("good-key".into(), JevModel::Latest, Duration::from_secs(1)).is_ok());
        assert!(Jev::for_test(
            "k".into(),
            JevModel::Pinned,
            Duration::from_secs(1),
            url::Url::parse("https://api.typesafe.ai/v1/systemone").unwrap()
        )
        .is_err());
    }
}

#[test]
fn configuration_maps_to_an_honest_status() {
    use crate::neev::config::RoutingConfig;
    assert_eq!(
        Router::from_config(&RoutingConfig::Off).status(),
        RouterStatus::Off
    );
    let keyless = Router::from_config(&RoutingConfig::Jev {
        api_key: None,
        model: JevModel::Pinned,
        timeout_ms: 3000,
        min_confidence: 0.3,
    });
    assert_eq!(keyless.status(), RouterStatus::MissingCredential);
    assert_eq!(keyless.name(), Some("jev"));
    let keyed = Router::from_config(&RoutingConfig::Jev {
        api_key: Some("configured-key".into()),
        model: JevModel::Pinned,
        timeout_ms: 3000,
        min_confidence: 0.3,
    });
    assert_eq!(keyed.status(), RouterStatus::Ready);
    assert_eq!(keyed.name(), Some("jev"));
}

#[test]
fn each_provider_is_capped_so_others_still_appear() {
    let mut codex = provider_capabilities("codex");
    codex.models = (0..40)
        .map(|n| model(&format!("m{n}"), &format!("M{n}"), None))
        .collect();
    let list = candidates(
        &[codex, provider_capabilities("claude")],
        &["codex", "claude"],
    );
    assert_eq!(
        list.iter().filter(|c| c.provider == "codex").count(),
        MAX_PER_PROVIDER
    );
    assert!(list.iter().any(|c| c.provider == "claude"));
}

#[test]
fn an_unsure_complexity_rounds_effort_up_not_down() {
    let all: Vec<String> = ["low", "medium", "high", "xhigh"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(effort_for(&all, 1.4, false).as_deref(), Some("medium"));
    assert_eq!(effort_for(&all, 1.4, true).as_deref(), Some("high"));
}

#[tokio::test]
async fn low_complexity_confidence_does_not_lower_the_effort() {
    let list = claude_candidates();
    let opus = list.iter().position(|c| c.model == "opus").unwrap();
    let fake = Fake::new(Ok(Decision {
        option: opus,
        confidence: 0.9,
        complexity: 1.2,
        complexity_confidence: 0.2,
    }));
    let route = ready(fake).route(ask(), &list, manual()).await;
    assert_eq!(route.selection.effort.as_deref(), Some("high"));
}

#[tokio::test]
async fn confidence_exactly_at_the_floor_is_accepted() {
    let fake = Fake::new(Ok(Decision {
        option: 0,
        confidence: 0.3,
        complexity: 1.0,
        complexity_confidence: 0.9,
    }));
    let route = ready(fake)
        .route(ask(), &claude_candidates(), manual())
        .await;
    assert_eq!(route.outcome, Outcome::Routed);
}

#[tokio::test]
async fn a_brief_follow_up_keeps_the_current_selection_without_asking() {
    for prompt in ["yes", "continue", "fix it", "ok, ship it", "はい、続けて"] {
        let fake = Fake::new(Ok(Decision {
            option: 0,
            confidence: 0.99,
            complexity: 0.0,
            complexity_confidence: 0.99,
        }));
        let route = ready(fake.clone())
            .route(
                Ask {
                    prompt,
                    attached_files: 0,
                    earlier_turns: 4,
                    previous: Some(Previous {
                        request: "Rewrite the scheduler",
                        reply: "Done; tests fail in two places",
                    }),
                },
                &claude_candidates(),
                manual(),
            )
            .await;
        assert_eq!(route.selection, manual(), "{prompt}");
        assert_eq!(route.outcome, Outcome::Kept);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_brief_first_message_is_still_routed() {
    let fake = Fake::new(Ok(Decision {
        option: 0,
        confidence: 0.9,
        complexity: 0.0,
        complexity_confidence: 0.9,
    }));
    let route = ready(fake.clone())
        .route(
            Ask {
                prompt: "hi",
                attached_files: 0,
                earlier_turns: 0,
                previous: None,
            },
            &claude_candidates(),
            manual(),
        )
        .await;
    assert_eq!(route.outcome, Outcome::Routed);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_substantive_follow_up_is_routed_with_bounded_previous_context() {
    let fake = Fake::new(Ok(Decision {
        option: 0,
        confidence: 0.9,
        complexity: 2.0,
        complexity_confidence: 0.9,
    }));
    let long_reply = "r".repeat(MAX_PREVIOUS_CHARS * 3);
    ready(fake.clone())
        .route(
            Ask {
                prompt: "Now also migrate the invoices table and backfill the new column safely",
                attached_files: 0,
                earlier_turns: 3,
                previous: Some(Previous {
                    request: "Add a currency column",
                    reply: &long_reply,
                }),
            },
            &claude_candidates(),
            manual(),
        )
        .await;
    let seen = fake.seen.lock().unwrap().clone().unwrap();
    let (request, reply) = seen.previous.unwrap();
    assert_eq!(request, "Add a currency column");
    assert!(reply.chars().count() <= MAX_PREVIOUS_CHARS + 32);
}

#[tokio::test]
async fn a_rejected_request_falls_back_with_its_own_reason() {
    let route = ready(Fake::new(Err(DecisionError::Rejected)))
        .route(ask(), &claude_candidates(), manual())
        .await;
    assert_eq!(route.outcome, Outcome::Fallback(FallbackReason::Rejected));
    assert_eq!(route.selection, manual());
}

#[tokio::test]
async fn a_routed_decision_carries_its_complexity_and_an_unsure_one_rounds_up() {
    for (complexity, confidence, expected) in [(2.2, 0.9, 2), (1.2, 0.2, 2), (0.4, 0.9, 0)] {
        let fake = Fake::new(Ok(Decision {
            option: 0,
            confidence: 0.9,
            complexity,
            complexity_confidence: confidence,
        }));
        let route = ready(fake)
            .route(ask(), &claude_candidates(), manual())
            .await;
        assert_eq!(
            route.complexity,
            Some(expected),
            "{complexity} {confidence}"
        );
    }
    // No decision, no complexity: a fallback never claims to know the work.
    let route = ready(Fake::new(Err(DecisionError::Timeout)))
        .route(ask(), &claude_candidates(), manual())
        .await;
    assert_eq!(route.complexity, None);
}

#[tokio::test]
async fn each_option_carries_only_its_usage_word() {
    let mut list = claude_candidates();
    list[0].remaining_usage = "low";
    let fake = Fake::new(Ok(Decision {
        option: 0,
        confidence: 0.9,
        complexity: 1.0,
        complexity_confidence: 0.9,
    }));
    ready(fake.clone()).route(ask(), &list, manual()).await;
    let seen = fake.seen.lock().unwrap().clone().unwrap();
    assert_eq!(seen.options[0].remaining_usage, "low");
    assert_eq!(seen.options[1].remaining_usage, "unknown");
}

#[test]
fn decisions_are_limited_per_person_and_overall_and_the_window_slides() {
    let limiter = DecisionLimiter::new();
    for _ in 0..DECISIONS_PER_USER_PER_MINUTE {
        assert!(limiter.admit(1, 1, 1_000));
    }
    assert!(!limiter.admit(1, 1, 1_000), "the person's limit");
    // Someone else, even in the same org, is unaffected.
    assert!(limiter.admit(1, 2, 1_000));
    // A minute later the window has moved on.
    assert!(limiter.admit(1, 1, 1_061));

    let limiter = DecisionLimiter::new();
    let mut admitted = 0;
    for user in 0..(DECISIONS_GLOBAL_PER_MINUTE as i32) {
        if limiter.admit(9, user, 5_000) {
            admitted += 1;
        }
    }
    assert_eq!(admitted, DECISIONS_GLOBAL_PER_MINUTE);
    assert!(
        !limiter.admit(9, 99_999, 5_000),
        "the deployment-wide limit"
    );
    assert!(limiter.admit(9, 99_999, 5_061));
}
