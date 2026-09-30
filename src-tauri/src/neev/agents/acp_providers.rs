//! The hosted ACP providers — Cursor, Gemini CLI, OpenCode — in one place.
//!
//! They run through `scripts/operations/neev_acp_stream_adapter.py` inside the
//! same sandbox, egress lane and per-user credential volume as Claude and Codex,
//! and the adapter speaks the Codex item dialect, so the activity parser reads
//! them with its Codex mapping.
//!
//! What they can be asked to do is deliberately narrow, and every "no" below is
//! a fact about the integration rather than a placeholder:
//!
//! - **Model** — the provider's own default only. OpenCode is additionally
//!   pinned by the adapter to the signed-in upstream (`openai/…`), because with
//!   no sign-in it silently answers on anonymous third-party models.
//! - **Effort / speed** — no flag exists on the ACP path for any of the three.
//! - **Images** — not sent (the adapter sends text blocks only); staged files
//!   are still named in the prompt like every attachment.
//! - **Steering** — none: stable ACP v1 has no mid-turn input.
//! - **Library MCP servers** — not forwarded (`session/new` gets none).
//!
//! **Not advertised by default.** A provider is offered to the product only when
//! the operator names it in `NEEV_AGENT_ACP_PROVIDERS` (comma-separated; empty or
//! unset = none). That must not happen before (a) the storage lane widens the
//! `provider IN ('claude','codex')` CHECKs, (b) its CLI is pinned in the image,
//! and (c) a real sign-in → run → stream → cancel smoke has passed.

use std::sync::OnceLock;

/// Every ACP provider id this build understands, in display order.
pub(crate) const ACP_PROVIDERS: &[&str] = &["cursor", "gemini", "opencode"];

/// The environment variable naming the ACP providers the product may offer.
pub(crate) const GATE_ENV: &str = "NEEV_AGENT_ACP_PROVIDERS";

pub(crate) fn is_acp(provider: &str) -> bool {
    ACP_PROVIDERS.contains(&provider)
}

/// The name the reader sees.
pub(crate) fn label(provider: &str) -> Option<&'static str> {
    match provider {
        "cursor" => Some("Cursor"),
        "gemini" => Some("Gemini CLI"),
        "opencode" => Some("OpenCode"),
        _ => None,
    }
}

/// Strict parse of the gate. An unknown or repeated id is an error, so a typo
/// aborts the boot instead of silently leaving a provider off (or on).
pub(crate) fn parse_gate(raw: &str) -> Result<Vec<&'static str>, ()> {
    let mut enabled: Vec<&'static str> = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|entry| !entry.is_empty()) {
        let known = ACP_PROVIDERS
            .iter()
            .copied()
            .find(|provider| *provider == entry)
            .ok_or(())?;
        if enabled.contains(&known) {
            return Err(());
        }
        enabled.push(known);
    }
    Ok(enabled)
}

static GATE: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Record the boot-time gate. Called once by startup; later calls are ignored.
pub(crate) fn configure_gate(enabled: Vec<&'static str>) {
    let _ = GATE.set(enabled);
}

/// Whether the product may offer `provider` (sign-in, sessions, runs, catalogue).
/// False for every provider until startup has configured the gate.
pub(crate) fn hosted_enabled(provider: &str) -> bool {
    GATE.get()
        .is_some_and(|enabled| enabled.iter().any(|entry| *entry == provider))
}

/// The ACP providers currently offered, in display order.
pub(crate) fn enabled() -> Vec<&'static str> {
    ACP_PROVIDERS
        .iter()
        .copied()
        .filter(|provider| hosted_enabled(provider))
        .collect()
}

/// The marker the launcher's `acp_stream_argv` requires between the binary and
/// the prompt. Never reaches a provider: the launcher replaces the whole argv.
pub(crate) const LAUNCH_MARKER: &str = "--neev-acp";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_is_strict_and_defaults_to_nothing() {
        assert_eq!(parse_gate(""), Ok(vec![]));
        assert_eq!(parse_gate(" , "), Ok(vec![]));
        assert_eq!(parse_gate("gemini"), Ok(vec!["gemini"]));
        assert_eq!(
            parse_gate("opencode, cursor"),
            Ok(vec!["opencode", "cursor"])
        );
        for bad in ["claude", "codex", "Gemini", "gemini,gemini", "grok", "cursor-agent"] {
            assert_eq!(parse_gate(bad), Err(()), "{bad}");
        }
    }

    #[test]
    fn only_the_three_acp_providers_are_acp() {
        for provider in ACP_PROVIDERS {
            assert!(is_acp(provider));
            assert!(label(provider).is_some());
        }
        for other in ["claude", "codex", "", "agent"] {
            assert!(!is_acp(other));
            assert!(label(other).is_none());
        }
    }

    #[test]
    fn nothing_is_offered_before_the_gate_is_configured() {
        // The process-wide gate is never configured in unit tests.
        for provider in ACP_PROVIDERS {
            assert!(!hosted_enabled(provider));
        }
        assert!(enabled().is_empty());
    }
}
