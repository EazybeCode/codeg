//! A provider's own failure, made safe to keep and specific enough to act on.
//!
//! A failed turn used to be stored as nothing but `failed`: the adapter's reason
//! was replaced by that literal, stderr went to `/dev/null`, and the reader saw
//! "This service is temporarily unavailable" for a Codex sign-in problem. This
//! module is the part in between: it keeps the provider's message, strips what
//! must never be stored or shown, bounds it, and says what KIND of failure it
//! was so the UI can offer the one action that helps (reconnect, or retry).
//!
//! Stored as a `status` activity item (`state: "error"`, `detail` = message,
//! `category` = the class). That is the existing bounded trace column, so no
//! schema change is needed, and the field is additive: an older reader ignores
//! it and an older client drops an unknown state.
//!
//! Only the provider's FAILURE text passes through here. Hidden reasoning never
//! reaches it — the parsers drop thinking before anything is mapped.

use std::sync::OnceLock;

use regex::Regex;

use super::activity::{ActivityDraft, ActivityKind};

/// Characters of a provider message kept after redaction.
pub(crate) const MAX_MESSAGE_CHARS: usize = 300;
/// The activity state an error item carries.
pub(crate) const STATE_ERROR: &str = "error";
/// The activity id an error item carries. One per turn: the accumulator appends,
/// and a turn fails once.
pub(crate) const ERROR_ID: &str = "error";

/// What kind of failure this was — the thing that decides the action offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    /// Signed in once, but the stored credential expired or could not refresh.
    AuthExpired,
    /// No usable sign-in at all, or the provider rejected the credential.
    SigninRequired,
    /// The account's plan/usage/billing limit is spent.
    Quota,
    /// Too many requests or the provider is overloaded right now.
    RateLimit,
    /// The connection to the provider failed or dropped.
    Network,
    /// Something took too long.
    Timeout,
    /// The provider refused THIS request (bad request, policy, context size,
    /// a setting it would not take). Retrying the same thing will not help.
    ProviderRefusal,
    /// Our runtime could not start or keep the provider running.
    RuntimeUnavailable,
    /// The provider failed and said nothing we can classify.
    Provider,
}

impl Category {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AuthExpired => "auth_expired",
            Self::SigninRequired => "signin_required",
            Self::Quota => "quota",
            Self::RateLimit => "rate_limit",
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::ProviderRefusal => "provider_refusal",
            Self::RuntimeUnavailable => "runtime_unavailable",
            Self::Provider => "provider",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auth_expired" => Self::AuthExpired,
            "signin_required" => Self::SigninRequired,
            "quota" => Self::Quota,
            "rate_limit" => Self::RateLimit,
            "network" => Self::Network,
            "timeout" => Self::Timeout,
            "provider_refusal" => Self::ProviderRefusal,
            "runtime_unavailable" => Self::RuntimeUnavailable,
            "provider" => Self::Provider,
            _ => return None,
        })
    }

    /// The turn's stored `error_code`. Kept inside the existing CHECK domain on
    /// purpose — the precise class lives on the activity item — so this needs no
    /// migration: an auth failure becomes `unauthorized` (which the UI already
    /// words as "sign in to your provider"), everything else stays `provider`.
    pub(crate) fn turn_error_code(self) -> &'static str {
        match self {
            Self::AuthExpired | Self::SigninRequired => "unauthorized",
            _ => "provider",
        }
    }
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static provider-error pattern"))
}

/// Classify from a structured hint first (Codex `codexErrorInfo`, Claude's
/// assistant `error`, the adapter's own code), then from the text.
pub(crate) fn classify(hint: Option<&str>, message: &str) -> Category {
    let text = message.to_ascii_lowercase();
    let expired = text.contains("expired")
        || text.contains("refresh token")
        || text.contains("refresh_token")
        || text.contains("could not be refreshed")
        || text.contains("re-authenticate")
        || text.contains("reauthenticate");
    if let Some(hint) = hint.map(|value| value.to_ascii_lowercase()) {
        let hint = hint.as_str();
        match hint {
            "unauthorized"
            | "authentication_failed"
            | "authentication_error"
            | "signin_required"
            | "auth" => {
                return if expired {
                    Category::AuthExpired
                } else {
                    Category::SigninRequired
                };
            }
            "auth_expired" => return Category::AuthExpired,
            "usagelimitexceeded"
            | "usage_limit_exceeded"
            | "billing_error"
            | "quota"
            | "insufficient_quota" => return Category::Quota,
            "serveroverloaded" | "server_overloaded" | "rate_limit" | "rate_limited"
            | "overloaded_error" => return Category::RateLimit,
            "httpconnectionfailed"
            | "responsestreamconnectionfailed"
            | "responsestreamdisconnected"
            | "responsetoomanyfailedattempts"
            | "network" => {
                return Category::Network;
            }
            "contextwindowexceeded"
            | "badrequest"
            | "invalid_request"
            | "provider_refusal"
            | "settings_mismatch"
            | "refused" => return Category::ProviderRefusal,
            "timeout" | "handshake_timeout" => return Category::Timeout,
            "sandboxerror" | "runtime_unavailable" | "closed" | "not_started" | "oversized" => {
                return Category::RuntimeUnavailable;
            }
            _ => {}
        }
    }
    static AUTH: OnceLock<Regex> = OnceLock::new();
    static QUOTA: OnceLock<Regex> = OnceLock::new();
    static RATE: OnceLock<Regex> = OnceLock::new();
    static TIMEOUT: OnceLock<Regex> = OnceLock::new();
    static NETWORK: OnceLock<Regex> = OnceLock::new();
    static REFUSAL: OnceLock<Regex> = OnceLock::new();
    static RUNTIME: OnceLock<Regex> = OnceLock::new();
    let auth = regex(
        &AUTH,
        r"\b401\b|unauthori[sz]ed|not logged in|please (run /login|log ?in|sign ?in)|log ?in again|sign ?in again|invalid (api[ _-]?key|x-api-key|token|credentials?)|authentication[_ ](error|failed|required)|no (credentials|auth)|login required|oauth token|access token|invalid_grant|refresh token",
    );
    if auth.is_match(&text) {
        return if expired {
            Category::AuthExpired
        } else {
            Category::SigninRequired
        };
    }
    if regex(
        &QUOTA,
        r"usage limit|usage_limit|insufficient_quota|exceeded your current quota|\bquota\b|credit balance|billing|hit your (usage |rate )?limit|limit (reached|will reset)|resets? (at|in) |plan limit|weekly limit|\d+-hour limit",
    )
    .is_match(&text)
    {
        return Category::Quota;
    }
    if regex(
        &RATE,
        r"rate[ _-]?limit|too many requests|\b429\b|\b529\b|overloaded|server is busy|capacity",
    )
    .is_match(&text)
    {
        return Category::RateLimit;
    }
    if regex(
        &TIMEOUT,
        r"timed? ?out|timeout|deadline exceeded|did not start the turn",
    )
    .is_match(&text)
    {
        return Category::Timeout;
    }
    if regex(
        &NETWORK,
        r"connection (refused|reset|closed|failed|error)|failed to connect|could not connect|network|dns|name resolution|stream (disconnected|error)|econn|unreachable|\btls\b|certificate|error sending request|reconnecting",
    )
    .is_match(&text)
    {
        return Category::Network;
    }
    if regex(
        &REFUSAL,
        r"\b400\b|bad request|invalid_request|context (window|length)|too long|content (policy|filter)|refus|not supported|unsupported model|model .*(not found|does not exist|not available)|provider ignored|invalid (value|argument|parameter)|unknown variant",
    )
    .is_match(&text)
    {
        return Category::ProviderRefusal;
    }
    if regex(
        &RUNTIME,
        r"did not start|closed the stream|stream ended|started no thread|oversized provider message|sandbox|spawn|no such file|permission denied|exited",
    )
    .is_match(&text)
    {
        return Category::RuntimeUnavailable;
    }
    Category::Provider
}

/// Make a provider message safe to store and show, and bound it.
///
/// Removes: values the server itself supplied (`needles`), bearer/API tokens,
/// JWTs, `key=value` secrets, long opaque strings, e-mail addresses, private
/// filesystem paths and URL query strings. Keeps the words that say what went
/// wrong. A JSON error body is reduced to its `message`, which is the part a
/// person can act on.
pub(crate) fn sanitize(message: &str, needles: &[String]) -> String {
    let mut text: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    for needle in needles {
        if needle.len() >= 8 && text.contains(needle.as_str()) {
            text = text.replace(needle.as_str(), "[redacted]");
        }
    }
    static INNER: OnceLock<Regex> = OnceLock::new();
    if let Some(inner) = regex(&INNER, r#""message"\s*:\s*"((?:[^"\\]|\\.){1,600})""#)
        .captures(&text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().replace("\\\"", "\"").replace("\\n", " "))
    {
        let prefix = text.split('{').next().unwrap_or("").trim().to_owned();
        text = if prefix.is_empty() {
            inner
        } else {
            format!("{prefix} {inner}")
        };
    }
    static FILE_URI: OnceLock<Regex> = OnceLock::new();
    static URL_QUERY: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static KEYED: OnceLock<Regex> = OnceLock::new();
    static JWT: OnceLock<Regex> = OnceLock::new();
    static PREFIXED: OnceLock<Regex> = OnceLock::new();
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static PATH: OnceLock<Regex> = OnceLock::new();
    static HOME: OnceLock<Regex> = OnceLock::new();
    static WINDOWS: OnceLock<Regex> = OnceLock::new();
    static OPAQUE: OnceLock<Regex> = OnceLock::new();
    static SPACE: OnceLock<Regex> = OnceLock::new();
    let rules: [(&Regex, &str); 11] = [
        // A `file:` URI is a local path whatever directory it names; the path
        // rule below needs a delimiter before `/Users`, which `file://` lacks.
        (regex(&FILE_URI, r#"(?i)\bfile://[^\s"'<>]*"#), "[path]"),
        (
            regex(&URL_QUERY, r#"(https?://[^\s"'?#<>]+)[?#][^\s"'<>]*"#),
            "$1",
        ),
        (
            regex(&BEARER, r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+"),
            "Bearer [redacted]",
        ),
        (
            regex(
                &KEYED,
                r#"(?i)\b(api[_-]?key|x-api-key|access[_-]?token|refresh[_-]?token|id[_-]?token|token|secret|password|passwd|authorization|cookie|session[_-]?id|client[_-]?secret)(["']?\s*[:=]\s*["']?)[^\s"',;}]+"#,
            ),
            "$1$2[redacted]",
        ),
        (
            regex(
                &JWT,
                r"\beyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*",
            ),
            "[redacted]",
        ),
        (
            regex(
                &PREFIXED,
                r"\b(sk|pk|rk|ghp|gho|ghu|ghs|xox[abp]|AKIA)[-_A-Za-z0-9]{8,}",
            ),
            "[redacted]",
        ),
        (
            regex(
                &EMAIL,
                r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
            ),
            "[email]",
        ),
        (
            regex(
                &PATH,
                r#"(^|[\s"'=(\[:,])/(home|Users|root|data|var|etc|opt|tmp|run|private|mnt|srv|proc|workspace|app)(/[^\s"'():,;\]]*)?"#,
            ),
            "$1[path]",
        ),
        (
            regex(&HOME, r#"(^|[\s"'=(\[:,])~/[^\s"'():,;\]]*"#),
            "$1[path]",
        ),
        (regex(&WINDOWS, r#"\b[A-Za-z]:\\[^\s"'():,;]*"#), "[path]"),
        (regex(&OPAQUE, r"[A-Za-z0-9_+/=-]{32,}"), "[redacted]"),
    ];
    for (pattern, replacement) in rules {
        text = pattern.replace_all(&text, replacement).into_owned();
    }
    let text = regex(&SPACE, r"\s+")
        .replace_all(text.trim(), " ")
        .into_owned();
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return text;
    }
    let mut short: String = text.chars().take(MAX_MESSAGE_CHARS - 1).collect();
    short.push('…');
    short
}

/// The activity step that records a provider failure.
pub(crate) fn draft(category: Category, message: String) -> ActivityDraft {
    ActivityDraft {
        id: ERROR_ID.to_owned(),
        kind: ActivityKind::Status {
            state: STATE_ERROR.to_owned(),
            detail: if message.is_empty() {
                None
            } else {
                Some(message)
            },
            category: Some(category.as_str().to_owned()),
        },
    }
}

/// The category an activity step records, if it is an error step.
pub(crate) fn category_of(kind: &ActivityKind) -> Option<Category> {
    match kind {
        ActivityKind::Status {
            state,
            category: Some(category),
            ..
        } if state == STATE_ERROR => Category::parse(category),
        _ => None,
    }
}

#[cfg(test)]
#[path = "provider_error_tests.rs"]
mod provider_error_tests;
