//! Which sources a finished run drew on, decided from its stored tool trace —
//! the gate between a member's run and a channel everyone reads.
//!
//! A channel run is briefed for publication (`LaunchAudience::Published`), but
//! a briefing is only an instruction: the run executes on the requester's
//! provider account, whose own connectors Neev cannot switch off per run, and
//! Neev has not proven that the run environment's file, shell or local network
//! tools are confined to channel-safe material. So the answer is PUBLISHED
//! only when its complete trace contains nothing but tools that READ no
//! source at all:
//! - `Write` (creates a deliverable from what the model already has),
//! - planning/todo tools, and
//! - server-side web search (`WebSearch`, Codex `web_search`): public results.
//!
//! Everything else is UNCLASSIFIED and the answer content is withheld from the
//! channel (the requester keeps it in their own Neev chat): file reads,
//! search and edits (`Read`, `Grep`, `Glob`, `Edit`, `apply_patch`…), shell
//! (`Bash`, `shell`…), locally executed fetches (`WebFetch`), sub-agents
//! (`Task`, whose inner calls are not in the trace), EVERY MCP tool — provider
//! account connectors and the org's shared library integrations alike, since a
//! shared integration's credential does not prove each returned object fits
//! the channel's audience — unknown tools, and a trace that was cut short.
//! Neev stores a complete trace for every run that used tools
//! (`activity::Log::stored` keeps a truncation marker when it must cut one),
//! so "no trace" means "no tools".

use crate::neev::agents::activity::{ActivityItem, ActivityKind, STATE_TRUNCATED};

/// Tools that read no source: they only write the run's own output, plan, or
/// search the public web from the provider's side.
const SOURCE_FREE: &[&str] = &["Write", "TodoWrite", "ExitPlanMode", "WebSearch", "update_plan", "web_search"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sources {
    /// No tool that reads a source (or no tools at all).
    SourceFree,
    /// Something the channel must not receive; the reason is a stable code.
    Unclassified(&'static str),
}

/// Classify a run from its complete stored trace.
pub(crate) fn classify(activity: &[ActivityItem]) -> Sources {
    for item in activity {
        match &item.kind {
            ActivityKind::Status { state, .. } if state == STATE_TRUNCATED => {
                return Sources::Unclassified("teams_trace_incomplete");
            }
            ActivityKind::ToolCall { name, .. } if !SOURCE_FREE.contains(&name.as_str()) => {
                return Sources::Unclassified("teams_unclassified_source");
            }
            _ => {}
        }
    }
    Sources::SourceFree
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> ActivityItem {
        ActivityItem {
            seq: 1,
            offset: 0,
            id: "t".to_owned(),
            kind: ActivityKind::ToolCall {
                name: name.to_owned(),
                input: "{}".to_owned(),
                input_truncated: false,
            },
        }
    }

    fn status(state: &str) -> ActivityItem {
        ActivityItem {
            seq: 2,
            offset: 0,
            id: "s".to_owned(),
            kind: ActivityKind::Status {
                state: state.to_owned(),
                detail: None,
                category: None,
            },
        }
    }

    #[test]
    fn only_source_free_runs_are_publishable() {
        assert_eq!(classify(&[]), Sources::SourceFree, "no tools at all");
        assert_eq!(classify(&[call("WebSearch"), call("Write"), call("TodoWrite"), status("running")]), Sources::SourceFree);
        assert_eq!(classify(&[call("web_search"), call("update_plan")]), Sources::SourceFree);
    }

    #[test]
    fn reads_shell_fetch_subagents_mcp_unknown_and_incomplete_are_withheld() {
        for name in [
            "Read", "Grep", "Glob", "Edit", "MultiEdit", "Bash", "BashOutput", "WebFetch", "Task",
            "shell", "exec_command", "apply_patch",
            // Provider-account connector and a shared library integration alike.
            "mcp__claude_ai_Gmail__search", "mcp__hubspot__search",
            "SomeNewTool", "",
        ] {
            assert_eq!(classify(&[call("Write"), call(name)]), Sources::Unclassified("teams_unclassified_source"), "{name:?}");
        }
        assert_eq!(classify(&[call("Write"), status(STATE_TRUNCATED)]), Sources::Unclassified("teams_trace_incomplete"));
    }
}
