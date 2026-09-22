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
