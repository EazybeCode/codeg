//! What each provider's CLI will actually honour.
//!
//! These assertions encode facts read off the DEPLOYED binaries, not a product
//! screenshot. The one that matters most is the refusal: `claude` accepts an
//! unknown `--effort` and runs at its default anyway, so anything this layer
//! waves through becomes a setting that appears to work and does nothing.

use super::*;

#[test]
fn an_unset_effort_is_allowed_and_sends_nothing() {
    // The provider's own default, which is the only default we are entitled to
    // pick on somebody else's behalf.
    assert_eq!(validated_effort("claude", None), Ok(None));
    assert_eq!(validated_effort("codex", None), Ok(None));
    // An empty string is the same statement made by a form that submitted a
    // blank field.
    assert_eq!(validated_effort("claude", Some("")), Ok(None));
}

#[test]
fn each_provider_accepts_only_the_levels_its_cli_takes() {
    // Quoted verbatim by claude's own validator: "Valid values: low, medium,
    // high, xhigh, max."
    for level in ["low", "medium", "high", "xhigh", "max"] {
        assert!(
            validated_effort("claude", Some(level)).is_ok(),
            "claude should accept {level}"
        );
    }
    // Claude's validator does not list `minimal`.
    assert!(validated_effort("claude", Some("minimal")).is_err());

    // Codex carries `minimal` and has no `max` among its values.
    for level in ["minimal", "low", "medium", "high", "xhigh"] {
        assert!(
            validated_effort("codex", Some(level)).is_ok(),
            "codex should accept {level}"
        );
    }
    assert!(validated_effort("codex", Some("max")).is_err());
}

#[test]
fn an_unknown_level_is_refused_rather_than_passed_on() {
    // The load-bearing case. `claude --effort bogus` warns and silently runs at
    // its default, so a value that reached the argv unchecked would present as a
    // working setting while changing nothing the reader asked for.
    assert!(validated_effort("claude", Some("bogus")).is_err());
    assert!(validated_effort("claude", Some("LOW")).is_err());
    assert!(validated_effort("codex", Some("extreme")).is_err());
    // An unknown PROVIDER supports nothing: better to refuse than to guess
    // which dialect to speak.
    assert!(validated_effort("gemini", Some("low")).is_err());
}

#[test]
fn the_catalogue_offers_only_levels_that_provider_supports() {
    for provider in catalogue().providers {
        for level in &provider.efforts {
            assert!(
                validated_effort(provider.provider, Some(level)).is_ok(),
                "{} advertised {level} but would refuse it",
                provider.provider
            );
        }
    }
}

#[test]
fn every_provider_has_exactly_one_default_model_and_it_is_the_cli_default() {
    for provider in catalogue().providers {
        let defaults: Vec<_> = provider
            .models
            .iter()
            .filter(|model| model.default)
            .collect();
        assert_eq!(
            defaults.len(),
            1,
            "{} needs exactly one default model",
            provider.provider
        );
        // `default` means "pass no --model", so a new chat cannot land on a more
        // expensive model because a catalogue entry was marked first.
        assert_eq!(defaults[0].id, "default");
    }
}

#[test]
fn a_catalogued_model_is_known_and_an_invented_one_is_not() {
    assert!(known_model("claude", "opus"));
    assert!(known_model("claude", "default"));
    // Names from another product's menu are not in the catalogue, because
    // nothing here has established that these CLI versions accept them.
    assert!(!known_model("claude", "fable-5.1"));
    assert!(!known_model("codex", "gpt-6-astra"));
}

#[test]
fn fast_is_allowed_only_where_the_model_can_actually_run_it() {
    // The pairing the whole per-model design exists to prevent.
    assert_eq!(validated_speed("claude", "opus", true), Ok(true));
    assert!(validated_speed("claude", "haiku", false).is_ok());
    assert!(
        validated_speed("claude", "haiku", true).is_err(),
        "Haiku cannot run Fast, and answering by quietly switching to Opus would \
         spend money on a model nobody chose"
    );
    assert!(validated_speed("claude", "sonnet", true).is_err());
    // The provider default resolves to whatever the CLI picks, so Fast beside it
    // would be a promise about a model we have not named.
    assert!(validated_speed("claude", "default", true).is_err());
    // A deployment-pinned full name has no established Fast support.
    assert!(validated_speed("claude", "claude-opus-4-5-20990101", true).is_err());
    // Off is always fine, whatever the model.
    assert_eq!(validated_speed("claude", "haiku", false), Ok(false));
}

#[test]
fn the_combined_validator_refuses_a_pairing_neither_field_shows_alone() {
    // Each half is individually fine — Haiku is a real model, Fast is a real
    // control — and together they are not. Validating them separately is how a
    // session ends up storing a combination no run can honour.
    assert!(validated_settings("claude", "haiku", Some("high"), true).is_err());
    assert_eq!(
        validated_settings("claude", "haiku", Some("high"), false),
        Ok((Some(Effort::High), false))
    );
    assert_eq!(
        validated_settings("claude", "opus", Some("max"), true),
        Ok((Some(Effort::Max), true))
    );
    // Provider-level refusals still apply through the combined path.
    assert!(validated_settings("claude", "opus", Some("minimal"), false).is_err());
    assert!(validated_settings("codex", "default", Some("max"), false).is_err());
    // And an id that could be read as a flag never reaches an argv.
    assert!(validated_settings("claude", "--oops", None, false).is_err());
}

#[test]
fn a_models_own_effort_list_narrows_its_providers() {
    // Every shipped entry declares `None` today, so this pins the branch that
    // goes live the moment discovery fills `supportedReasoningEfforts` in — the
    // point at which nobody would think to re-check it.
    const NARROW: &[&str] = &["low", "medium"];
    let narrowed = Model {
        id: "narrow-model",
        label: "Narrow",
        default: false,
        supports_speed: false,
        efforts: Some(NARROW),
    };
    assert!(narrowed.efforts.is_some_and(|list| list.contains(&"low")));
    assert!(narrowed.efforts.is_some_and(|list| !list.contains(&"xhigh")));

    // And the provider-wide path still accepts what the provider accepts, so a
    // model with no declared list is not narrowed by accident.
    assert!(validated_effort_for("claude", "opus", Some("xhigh")).is_ok());
    assert!(validated_effort_for("claude", "opus", None).is_ok());
    // An unknown model id is not narrowed either — the CLI decides it.
    assert!(validated_effort_for("claude", "some-pinned-name", Some("high")).is_ok());
}

#[test]
fn the_single_field_effort_path_sees_the_same_rules_as_the_combined_one() {
    // `validated_effort_for` is what the PATCH /effort route calls. It must not
    // be a weaker door into the same store than PATCH /settings.
    for level in ["low", "medium", "high", "xhigh", "max"] {
        assert_eq!(
            validated_effort_for("claude", "opus", Some(level)).is_ok(),
            validated_settings("claude", "opus", Some(level), false).is_ok(),
            "the two paths disagreed about {level}"
        );
    }
    assert!(validated_effort_for("claude", "opus", Some("minimal")).is_err());
    assert!(validated_effort_for("codex", "default", Some("max")).is_err());
}

/// A probe's lane and permit must not come back before its teardown finishes.
///
/// The shape under test is the one discovery uses: the work is owned by a
/// spawned task holding the guards, awaited through a join handle. Dropping that
/// handle — which is what cancelling the request does — must not release
/// anything early, because the next run would otherwise take a slot whose
/// sandbox is still unwinding.
#[tokio::test]
async fn cancelling_a_probe_does_not_release_its_slot_until_teardown_finishes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let permits = Arc::new(tokio::sync::Semaphore::new(1));
    let torn_down = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));

    struct Guard {
        released: Arc<AtomicBool>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    let permit = permits.clone().try_acquire_owned().expect("first permit");
    let guard = Guard {
        released: released.clone(),
    };
    let teardown = torn_down.clone();
    let task = tokio::spawn(async move {
        // Stands in for the launcher's SIGTERM grace.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        teardown.store(true, Ordering::SeqCst);
        drop(guard);
        drop(permit);
    });

    // The caller goes away mid-probe.
    drop(task);
    assert!(
        permits.try_acquire().is_err(),
        "the slot is still held while the probe tears down"
    );
    assert!(!released.load(Ordering::SeqCst));

    // The detached task runs to completion regardless.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert!(
        torn_down.load(Ordering::SeqCst),
        "teardown still ran after the caller was cancelled"
    );
    assert!(released.load(Ordering::SeqCst), "and only then released");
    assert!(
        permits.try_acquire().is_ok(),
        "the slot is reusable once, and only once, teardown has finished"
    );
}

#[test]
fn acp_providers_claim_nothing_they_cannot_do() {
    for provider in ["cursor", "gemini", "opencode"] {
        let caps = provider_capabilities(provider);
        assert_ne!(caps.label, "Agent");
        assert!(caps.efforts.is_empty(), "{provider}: no effort control");
        assert!(!caps.supports_speed, "{provider}: no speed control");
        assert_eq!(caps.models.len(), 1);
        assert_eq!(caps.models[0].id, "default");
        assert!(!caps.models[0].supports_speed);
        assert!(validated_settings(provider, "default", None, false).is_ok());
        assert!(validated_settings(provider, "gpt-5", None, false).is_err());
        assert!(validated_settings(provider, "default", Some("high"), false).is_err());
        assert!(validated_settings(provider, "default", None, true).is_err());
    }
}

#[test]
fn acp_providers_are_not_advertised_until_gated_on() {
    let listed: Vec<&str> = catalogue().providers.iter().map(|p| p.provider).collect();
    assert_eq!(listed, vec!["claude", "codex"]);
}

#[test]
fn attachments_and_library_tools_are_described_truthfully() {
    assert_eq!(provider_capabilities("codex").image_input, "native");
    assert_eq!(provider_capabilities("claude").image_input, "file");
    for provider in ["claude", "codex", "cursor", "gemini", "opencode"] {
        let caps = provider_capabilities(provider);
        // HTTP tool hosts are outside sandbox egress: only stdio tools work.
        assert_eq!(caps.library_mcp, "stdio", "{provider}");
        let json = serde_json::to_value(&caps).unwrap();
        assert!(json.get("imageInput").is_some() && json.get("libraryMcp").is_some());
    }
    for provider in ["cursor", "gemini", "opencode"] {
        // Decided per run from the agent's own initialize, never promised.
        assert_eq!(provider_capabilities(provider).image_input, "native_if_supported");
    }
}
