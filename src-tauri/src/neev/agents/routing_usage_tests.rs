use serde_json::json;

use super::*;

const NOW: i64 = 1_790_000_000;

fn key(user_id: UserId, provider: &str) -> AccountKey {
    AccountKey {
        org_id: 1,
        user_id,
        provider: provider.to_owned(),
        generation: 1,
    }
}

fn window(used: f64, resets_at: Option<i64>) -> Window {
    Window {
        used_percent: used,
        window_minutes: Some(300),
        resets_at,
    }
}

fn snapshot(windows: Vec<Window>) -> Snapshot {
    Snapshot {
        pools: vec![Pool {
            id: "codex".to_owned(),
            windows,
        }],
    }
}

fn candidate(provider: &'static str, model: &str) -> Candidate {
    Candidate {
        provider,
        provider_label: provider,
        model: model.to_owned(),
        label: model.to_owned(),
        efforts: Vec::new(),
        remaining_usage: "unknown",
    }
}

// ---- Codex parsing ----

#[test]
fn codex_multi_bucket_response_keeps_only_windows() {
    let value = json!({
        "rateLimits": {"limitId": "codex", "primary": {"usedPercent": 1.0}},
        "rateLimitsByLimitId": {
            "codex": {
                "limitId": "codex",
                "limitName": "Codex",
                "primary": {"usedPercent": 42.5, "windowDurationMins": 300, "resetsAt": NOW + 3_600},
                "secondary": {"usedPercent": 10.0, "windowDurationMins": 10_080, "resetsAt": NOW + 86_400},
                "credits": {"hasCredits": true, "unlimited": false, "balance": "12.00"},
                "planType": "plus"
            }
        }
    });
    let parsed = parse_codex_rate_limits(&value, NOW).unwrap();
    assert_eq!(parsed.pools.len(), 1);
    let pool = &parsed.pools[0];
    assert_eq!(pool.id, "codex");
    assert_eq!(
        pool.windows[0],
        Window {
            used_percent: 42.5,
            window_minutes: Some(300),
            resets_at: Some(NOW + 3_600),
        }
    );
    assert_eq!(pool.windows[1].window_minutes, Some(10_080));
}

#[test]
fn codex_single_bucket_notification_is_read() {
    let value =
        json!({"rateLimits": {"primary": {"usedPercent": 5, "resetsAt": null}, "secondary": null}});
    let parsed = parse_codex_rate_limits(&value, NOW).unwrap();
    assert_eq!(parsed.pools[0].id, "default");
    assert_eq!(
        parsed.pools[0].windows,
        vec![Window {
            used_percent: 5.0,
            window_minutes: None,
            resets_at: None,
        }]
    );
}

#[test]
fn codex_implausible_values_are_refused_not_guessed() {
    // Negative or missing usage: no window at all.
    assert_eq!(
        parse_codex_rate_limits(
            &json!({"rateLimits": {"primary": {"usedPercent": -1}}}),
            NOW
        ),
        None
    );
    assert_eq!(
        parse_codex_rate_limits(&json!({"rateLimits": {"primary": {"resetsAt": NOW}}}), NOW),
        None
    );
    assert_eq!(parse_codex_rate_limits(&json!({}), NOW), None);
    // Over 100 is spent, not more than spent; a far-off reset is unknown.
    let parsed = parse_codex_rate_limits(
        &json!({"rateLimits": {"primary": {
            "usedPercent": 140, "resetsAt": NOW + MAX_RESET_HORIZON_SECS + 1, "windowDurationMins": 0
        }}}),
        NOW,
    )
    .unwrap();
    assert_eq!(parsed.pools[0].windows[0], window_none(100.0));
    // A pool id that is not a plain identifier is dropped.
    assert_eq!(
        parse_codex_rate_limits(
            &json!({"rateLimitsByLimitId": {"a b<script>": {"primary": {"usedPercent": 1}}}}),
            NOW
        ),
        None
    );
}

fn window_none(used: f64) -> Window {
    Window {
        used_percent: used,
        window_minutes: None,
        resets_at: None,
    }
}

#[test]
fn codex_pool_count_is_bounded() {
    let mut by_id = serde_json::Map::new();
    for index in 0..(MAX_POOLS + 5) {
        by_id.insert(format!("p{index}"), json!({"primary": {"usedPercent": 1}}));
    }
    let parsed = parse_codex_rate_limits(&json!({"rateLimitsByLimitId": by_id}), NOW).unwrap();
    assert_eq!(parsed.pools.len(), MAX_POOLS);
}

// ---- Structured failures only ----

#[test]
fn only_provider_structured_quota_hints_count() {
    assert_eq!(
        structured_failure("codex", Some("usageLimitExceeded")),
        Some(FailureSignal::QuotaExhausted)
    );
    assert_eq!(
        structured_failure("claude", Some("rate_limit")),
        Some(FailureSignal::RateLimited)
    );
    assert_eq!(
        structured_failure("claude", Some("billing_error")),
        Some(FailureSignal::QuotaExhausted)
    );
    for (provider, hint) in [
        // Not this account's quota.
        ("codex", Some("serverOverloaded")),
        ("claude", Some("overloaded")),
        ("claude", Some("server_error")),
        // Auth and transport are never quota.
        ("codex", Some("unauthorized")),
        ("claude", Some("authentication_failed")),
        ("codex", Some("httpConnectionFailed")),
        ("codex", Some("responseStreamDisconnected")),
        // Our adapters' own codes.
        ("claude", Some("closed")),
        ("codex", Some("timeout")),
        ("codex", Some("not_started")),
        // A hint from the wrong provider, or none at all.
        ("claude", Some("usageLimitExceeded")),
        ("codex", Some("rate_limit")),
        ("gemini", Some("rate_limit")),
        ("codex", None),
        // Text is not a hint.
        ("codex", Some("You've hit your usage limit")),
    ] {
        assert_eq!(
            structured_failure(provider, hint),
            None,
            "{provider} {hint:?}"
        );
    }
}

// ---- The book ----

#[test]
fn an_unseen_account_is_unknown_and_available() {
    let book = UsageBook::new();
    assert_eq!(book.view(&key(1, "codex"), NOW), ProviderUsage::UNKNOWN);
}

#[test]
fn fresh_snapshot_sets_band_and_next_reset() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    book.record_snapshot(&account, snapshot(vec![window(20.0, Some(NOW + 60))]), NOW);
    let usage = book.view(&account, NOW);
    assert_eq!(usage.band, Band::Plenty);
    assert_eq!(usage.availability, Availability::Available);
    assert_eq!(usage.next_reset_at, Some(NOW + 60));
    assert_eq!(usage.observed_at, Some(NOW));

    // The most-used window decides.
    book.record_snapshot(
        &account,
        snapshot(vec![window(20.0, None), window(75.0, Some(NOW + 600))]),
        NOW,
    );
    let usage = book.view(&account, NOW);
    assert_eq!(usage.band, Band::Limited);
    assert_eq!(usage.next_reset_at, Some(NOW + 600));
    book.record_snapshot(&account, snapshot(vec![window(95.0, None)]), NOW);
    assert_eq!(book.view(&account, NOW).band, Band::Low);
}

#[test]
fn stale_or_reset_data_becomes_unknown() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    book.record_snapshot(&account, snapshot(vec![window(80.0, None)]), NOW);
    assert_eq!(
        book.view(&account, NOW + SNAPSHOT_TTL_SECS).band,
        Band::Unknown
    );
    assert_eq!(
        book.view(&account, NOW + SNAPSHOT_TTL_SECS).observed_at,
        None
    );

    book.record_snapshot(&account, snapshot(vec![window(80.0, Some(NOW + 30))]), NOW);
    assert_eq!(book.view(&account, NOW + 10).band, Band::Limited);
    // Past its reset the window says nothing — not "0% used".
    assert_eq!(book.view(&account, NOW + 30).band, Band::Unknown);
}

#[test]
fn a_spent_window_withholds_until_its_reported_reset_even_when_stale() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    let reset = NOW + 3 * 60 * 60;
    book.record_snapshot(&account, snapshot(vec![window(100.0, Some(reset))]), NOW);
    let later = NOW + SNAPSHOT_TTL_SECS + 60;
    assert_eq!(
        book.view(&account, later).availability,
        Availability::Exhausted { until: reset }
    );
    assert_eq!(
        book.view(&account, reset).availability,
        Availability::Available
    );
}

#[test]
fn a_spent_window_without_reset_holds_only_while_fresh() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    book.record_snapshot(&account, snapshot(vec![window(100.0, None)]), NOW);
    assert_eq!(
        book.view(&account, NOW).availability,
        Availability::ExhaustedUnknownReset {
            retry_after: NOW + SNAPSHOT_TTL_SECS
        }
    );
    assert_eq!(
        book.view(&account, NOW + SNAPSHOT_TTL_SECS).availability,
        Availability::Available
    );
}

#[test]
fn verified_rate_limits_back_off_and_cap_and_success_clears() {
    let book = UsageBook::new();
    let account = key(1, "claude");
    book.record_failure(&account, FailureSignal::RateLimited, NOW);
    assert_eq!(
        book.view(&account, NOW).availability,
        Availability::RateLimited {
            until: NOW + RATE_LIMIT_BASE_COOLDOWN_SECS
        }
    );
    book.record_failure(&account, FailureSignal::RateLimited, NOW);
    assert_eq!(
        book.view(&account, NOW).availability,
        Availability::RateLimited {
            until: NOW + 2 * RATE_LIMIT_BASE_COOLDOWN_SECS
        }
    );
    for _ in 0..40 {
        book.record_failure(&account, FailureSignal::RateLimited, NOW);
    }
    assert_eq!(
        book.view(&account, NOW).availability,
        Availability::RateLimited {
            until: NOW + RATE_LIMIT_MAX_COOLDOWN_SECS
        }
    );
    book.record_success(&account, NOW + 1);
    assert_eq!(
        book.view(&account, NOW + 1).availability,
        Availability::Available
    );
    // The back-off starts over.
    book.record_failure(&account, FailureSignal::RateLimited, NOW + 2);
    assert_eq!(
        book.view(&account, NOW + 2).availability,
        Availability::RateLimited {
            until: NOW + 2 + RATE_LIMIT_BASE_COOLDOWN_SECS
        }
    );
}

#[test]
fn quota_failure_without_reset_is_a_bounded_reprobe_cleared_by_new_room() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    book.record_failure(&account, FailureSignal::QuotaExhausted, NOW);
    assert_eq!(
        book.view(&account, NOW).availability,
        Availability::ExhaustedUnknownReset {
            retry_after: NOW + QUOTA_UNKNOWN_RESET_SECS
        }
    );
    assert_eq!(
        book.view(&account, NOW + QUOTA_UNKNOWN_RESET_SECS)
            .availability,
        Availability::Available
    );
    // A fresh report with room clears it ...
    book.record_failure(&account, FailureSignal::QuotaExhausted, NOW);
    book.record_snapshot(&account, snapshot(vec![window(30.0, None)]), NOW + 5);
    assert_eq!(
        book.view(&account, NOW + 5).availability,
        Availability::Available
    );
    // ... but not a rate-limit hold, which plan windows do not describe.
    book.record_failure(&account, FailureSignal::RateLimited, NOW + 6);
    book.record_snapshot(&account, snapshot(vec![window(30.0, None)]), NOW + 7);
    assert!(book.view(&account, NOW + 7).availability.is_blocked());
}

#[test]
fn success_clears_a_reported_spent_window() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    book.record_snapshot(
        &account,
        snapshot(vec![window(100.0, Some(NOW + 3_600)), window(40.0, None)]),
        NOW,
    );
    assert!(book.view(&account, NOW).availability.is_blocked());
    book.record_success(&account, NOW + 1);
    let usage = book.view(&account, NOW + 1);
    assert_eq!(usage.availability, Availability::Available);
    assert_eq!(usage.band, Band::Plenty);
}

#[test]
fn accounts_are_isolated_by_org_user_provider_and_generation() {
    let book = UsageBook::new();
    let account = key(1, "codex");
    book.record_failure(&account, FailureSignal::QuotaExhausted, NOW);
    let others = [
        AccountKey {
            org_id: 2,
            ..account.clone()
        },
        AccountKey {
            user_id: 2,
            ..account.clone()
        },
        AccountKey {
            provider: "claude".to_owned(),
            ..account.clone()
        },
        // A reconnect is a new sign-in.
        AccountKey {
            generation: 2,
            ..account.clone()
        },
    ];
    for other in &others {
        assert_eq!(book.view(other, NOW), ProviderUsage::UNKNOWN, "{other:?}");
    }
    book.forget(1, 1, "codex");
    assert_eq!(book.view(&account, NOW), ProviderUsage::UNKNOWN);
}

#[test]
fn the_book_is_bounded() {
    let book = UsageBook::new();
    for user in 0..(MAX_ACCOUNTS as i32 + 10) {
        book.record_failure(
            &key(user, "codex"),
            FailureSignal::RateLimited,
            NOW + i64::from(user),
        );
    }
    assert_eq!(book.len(), MAX_ACCOUNTS);
    // The least recently touched went first.
    assert_eq!(book.view(&key(0, "codex"), NOW), ProviderUsage::UNKNOWN);
}

// ---- Routing helpers ----

fn lookup<'a>(book: &'a UsageBook, now: i64) -> impl Fn(&str) -> ProviderUsage + 'a {
    move |provider: &str| book.view(&key(1, provider), now)
}

#[test]
fn auto_withholds_blocked_providers_and_keeps_the_rest_in_order() {
    let book = UsageBook::new();
    book.record_failure(&key(1, "codex"), FailureSignal::QuotaExhausted, NOW);
    let candidates = vec![
        candidate("claude", "sonnet"),
        candidate("codex", "gpt-a"),
        candidate("codex", "gpt-b"),
        candidate("claude", "opus"),
    ];
    let filtered = filter_for_auto(&candidates, lookup(&book, NOW));
    assert_eq!(
        filtered.eligible,
        vec![candidate("claude", "sonnet"), candidate("claude", "opus")]
    );
    assert_eq!(
        filtered.withheld,
        vec![Withheld {
            provider: "codex",
            availability: Availability::ExhaustedUnknownReset {
                retry_after: NOW + QUOTA_UNKNOWN_RESET_SECS
            },
        }]
    );

    // Everything withheld: nothing is invented; the caller falls back.
    book.record_failure(&key(1, "claude"), FailureSignal::RateLimited, NOW);
    let filtered = filter_for_auto(&candidates, lookup(&book, NOW));
    assert!(filtered.eligible.is_empty());
    assert_eq!(filtered.withheld.len(), 2);

    // Unknown usage is not a reason to withhold.
    let empty = UsageBook::new();
    assert_eq!(
        filter_for_auto(&candidates, lookup(&empty, NOW)).eligible,
        candidates
    );
}

#[test]
fn providers_are_ordered_by_headroom_with_unknown_in_the_middle() {
    let book = UsageBook::new();
    book.record_snapshot(&key(1, "codex"), snapshot(vec![window(95.0, None)]), NOW);
    book.record_snapshot(&key(1, "gemini"), snapshot(vec![window(5.0, None)]), NOW);
    let ordered = order_providers(
        &["codex", "claude", "opencode", "gemini"],
        lookup(&book, NOW),
    );
    assert_eq!(ordered, vec!["gemini", "claude", "opencode", "codex"]);
}

#[test]
fn a_decision_provider_sees_a_word_never_a_number_or_time() {
    for band in [Band::Unknown, Band::Plenty, Band::Limited, Band::Low] {
        let usage = ProviderUsage {
            band,
            ..ProviderUsage::UNKNOWN
        };
        let hint = decision_hint(&usage);
        assert!(hint.chars().all(|c| c.is_ascii_lowercase()), "{hint}");
    }
}

#[test]
fn manual_selection_gets_an_advisory_only() {
    let book = UsageBook::new();
    assert_eq!(manual_advisory(&book.view(&key(1, "codex"), NOW)), None);
    book.record_failure(&key(1, "codex"), FailureSignal::RateLimited, NOW);
    assert_eq!(
        manual_advisory(&book.view(&key(1, "codex"), NOW)),
        Some(Availability::RateLimited {
            until: NOW + RATE_LIMIT_BASE_COOLDOWN_SECS
        })
    );
}

#[test]
fn the_summary_carries_no_pool_or_plan_detail() {
    let book = UsageBook::new();
    book.record_snapshot(
        &key(1, "codex"),
        snapshot(vec![window(75.0, Some(NOW + 60))]),
        NOW,
    );
    let summary = summary("codex", &book.view(&key(1, "codex"), NOW));
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(
        value,
        json!({
            "provider": "codex",
            "availability": "available",
            "until": null,
            "remaining": "limited",
            "nextResetAt": NOW + 60,
            "observedAt": NOW
        })
    );
}
