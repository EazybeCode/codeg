//! What the installed provider CLIs can actually be asked to do.
//!
//! Every entry here was verified against the DEPLOYED runner binaries rather
//! than taken from a product screenshot, because the cost of being wrong is not
//! cosmetic. Two of the three controls are real; one is not, and says so.
//!
//! - **Model** — `claude --model <alias|name>`, `codex -m <name>`.
//! - **Effort** — `claude --effort <level>` (native, added between 2.1.247 and
//!   2.1.267) and `codex -c model_reasoning_effort=<level>`.
//! - **Speed / "Fast"** — real on both, and NOT a flag, which is why grepping
//!   `--help` for one is misleading. Claude takes it in settings
//!   (`--settings '{"fastMode":true}'`, non-interactive since 2.1.205; deployed
//!   is 2.1.267) and supports it on Opus only. Codex takes it as two config
//!   overrides (`service_tier="fast"`, `features.fast_mode=true`); the pinned
//!   binary's own app-server schema carries `service_tier`.
//!
//! Fast is per-MODEL, not per-provider: offering it beside a model that cannot
//! run it would either fail or silently switch the model, and switching the
//! model underneath somebody's selection is the worse of the two.
//!
//! The validation here is load-bearing for a specific reason: `claude` does not
//! reject an unknown effort. It warns and silently proceeds at the default —
//! "Unknown --effort value 'bogus' — ignoring it and using the default effort.
//! Valid values: low, medium, high, xhigh, max." A request that reached the CLI
//! unvalidated would therefore appear to work while running at an effort the
//! reader did not choose, which is precisely the outcome to prevent.

use serde::Serialize;

/// Effort levels, ordered from least to most.
///
/// One vocabulary across providers so the UI has a single scale, with support
/// declared per provider rather than assumed: Codex has `minimal` and no `max`;
/// Claude has `max` and no `minimal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    /// Whether THIS provider's CLI accepts this level.
    ///
    /// Claude's set is quoted verbatim by its own validator. Codex's comes from
    /// the values present in the installed binary; `max` is not among them, so
    /// it is not offered for Codex rather than sent hopefully.
    pub(crate) fn supported_by(self, provider: &str) -> bool {
        match provider {
            "claude" => matches!(
                self,
                Self::Low | Self::Medium | Self::High | Self::XHigh | Self::Max
            ),
            "codex" => matches!(
                self,
                Self::Minimal | Self::Low | Self::Medium | Self::High | Self::XHigh
            ),
            _ => false,
        }
    }
}

/// Every level, in scale order, for building a catalogue.
const ALL_EFFORTS: &[Effort] = &[
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::XHigh,
    Effort::Max,
];

/// A model the UI may offer.
///
/// Entries come from Claude's published model-configuration documentation and
/// its own `--model` help, which take either an alias or a full id. An id that
/// appears only in another product's menu is not reproduced here: an id the
/// runner rejects is a failed run, and a catalogue is a promise that it will
/// not be.
///
/// A deployment can still pin a full model name through the session's `model`
/// field. That path is validated for argv safety, not for existence, so the
/// CLI is the one that decides and its refusal surfaces as a failed turn rather
/// than as a wrong-model success.
///
/// Availability is an account matter. Listing a model says the CLI accepts the
/// id, not that a given provider account is entitled to it — the UI should read
/// these as selectable, not as guaranteed.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Model {
    /// What is sent to the CLI.
    pub(crate) id: &'static str,
    /// What the reader sees.
    pub(crate) label: &'static str,
    /// True for the entry used when nobody has chosen: the provider's own
    /// default, so a new chat cannot land on a more expensive model by
    /// accident.
    pub(crate) default: bool,
    /// Whether THIS model can run in fast mode. Claude documents Fast for Opus
    /// only; Sonnet and Haiku do not support it.
    pub(crate) supports_speed: bool,
    /// Effort levels THIS model accepts, when they are narrower than the
    /// provider's.
    ///
    /// `None` means "the provider's list applies" — not "all of them are
    /// certainly fine". Claude's CLI reports one set for the whole provider and
    /// nothing per model, so narrowing Haiku here would be a guess dressed as a
    /// capability. Codex's app-server DOES report `supportedReasoningEfforts`
    /// per model, so entries discovered from it will carry a real list.
    pub(crate) efforts: Option<&'static [&'static str]>,
}

const CLAUDE_MODELS: &[Model] = &[
    Model {
        id: "default",
        label: "Default",
        default: true,
        // The default resolves to whatever the CLI picks, which may or may not
        // be an Opus. Offering Fast here would be a promise about a model we
        // have not named.
        supports_speed: false,
        efforts: None,
    },
    Model {
        id: "haiku",
        label: "Haiku",
        default: false,
        supports_speed: false,
        efforts: None,
    },
    Model {
        id: "sonnet",
        label: "Sonnet",
        default: false,
        supports_speed: false,
        efforts: None,
    },
    Model {
        id: "opus",
        label: "Opus",
        default: false,
        // Fast mode is documented for Opus.
        supports_speed: true,
        efforts: None,
    },
    // Fable is a documented alias, not a Conductor-only name: `fable` resolves
    // to the latest and `claude-fable-5-1` pins it. It needs CLI >= 2.1.257 and
    // the deployed runner is 2.1.267, so the id is accepted here.
    Model {
        id: "fable",
        label: "Fable 5.1",
        default: false,
        // Fast is documented for Opus; claiming it for Fable would be an
        // assumption, and a wrong one costs a failed run or a silent downgrade.
        supports_speed: false,
        efforts: None,
    },
    Model {
        id: "claude-fable-5",
        label: "Fable 5",
        default: false,
        supports_speed: false,
        efforts: None,
    },
];

/// Codex documents `-m <MODEL>` without aliases and offers no listing, so the
/// only honest catalogue is its own default plus whatever a deployment pins.
/// Codex's real catalogue is discovered at runtime, not declared here.
///
/// Its app-server exposes `model/list` with `displayName`,
/// `supportedReasoningEfforts`, `defaultReasoningEffort` and `isDefault`, and
/// the protocol schema types a reasoning effort as "a non-empty reasoning effort
/// value advertised by the model" — an open string, per model. So a fixed list
/// in this file would be wrong by construction as soon as the pin moves.
///
/// Until that discovery is wired (see `catalogue`), this is the provider default
/// alone: honest about what has been established rather than a guess dressed as
/// a menu. Codex's speed controls are provider-wide rather than per-model.
const CODEX_MODELS: &[Model] = &[Model {
    id: "default",
    label: "Default",
    default: true,
    supports_speed: true,
    efforts: None,
}];

pub(crate) fn models(provider: &str) -> &'static [Model] {
    match provider {
        "claude" => CLAUDE_MODELS,
        "codex" => CODEX_MODELS,
        _ => &[],
    }
}

/// One model as the UI receives it. Owned, because a discovered catalogue is
/// not a compile-time constant.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelDto {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) default: bool,
    pub(crate) supports_speed: bool,
    /// `null` = the provider list applies. `[]` = this model supports none.
    pub(crate) efforts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default_effort: Option<String>,
}

/// One provider's controls, as the UI should render them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderCapabilities {
    pub(crate) provider: &'static str,
    pub(crate) label: &'static str,
    pub(crate) models: Vec<ModelDto>,
    /// Effort levels this provider accepts, least to most.
    ///
    /// When models are discovered this is the UNION of what they advertise, so
    /// every model's own list is a subset of it — the invariant the client
    /// parser relies on to render a narrowed model against the provider scale.
    pub(crate) efforts: Vec<String>,
    /// The level used when none is chosen. `None` means "the CLI's own
    /// default", which is what an unset session gets — no flag is passed at all.
    pub(crate) default_effort: Option<&'static str>,
    /// Whether this provider has a speed control at all. Per-model support is
    /// on each [`Model`], because Claude's Fast is Opus-only.
    pub(crate) supports_speed: bool,
    /// Shown when a chosen model cannot run fast, so the reason is visible
    /// rather than the control silently missing.
    pub(crate) speed_unavailable_reason: Option<&'static str>,
    /// Why this provider's catalogue is the built-in one rather than the
    /// account's own. `null` when the list came from the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) discovery_unavailable_reason: Option<String>,
}

fn label(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude Code",
        "codex" => "Codex",
        _ => "Agent",
    }
}

/// The built-in entries as DTOs, for a provider with nothing discovered.
fn static_models(provider: &str) -> Vec<ModelDto> {
    models(provider)
        .iter()
        .map(|model| ModelDto {
            id: model.id.to_owned(),
            label: model.label.to_owned(),
            default: model.default,
            supports_speed: model.supports_speed,
            efforts: model
                .efforts
                .map(|list| list.iter().map(|effort| (*effort).to_owned()).collect()),
            default_effort: None,
        })
        .collect()
}

fn static_efforts(provider: &str) -> Vec<String> {
    ALL_EFFORTS
        .iter()
        .filter(|effort| effort.supported_by(provider))
        .map(|effort| effort.name().to_owned())
        .collect()
}

pub(crate) fn provider_capabilities(provider: &'static str) -> ProviderCapabilities {
    ProviderCapabilities {
        provider,
        label: label(provider),
        models: static_models(provider),
        efforts: static_efforts(provider),
        // Unset, deliberately: passing no flag leaves the provider on its own
        // default, and a default we invented would be a silent opinion about
        // someone's token spend.
        default_effort: None,
        supports_speed: true,
        speed_unavailable_reason: Some(match provider {
            "claude" => "Fast mode runs on Opus. Choose Opus to enable it.",
            _ => "Fast mode is not available for this model.",
        }),
        discovery_unavailable_reason: None,
    }
}

/// The caller's OWN catalogue for one provider, discovered where possible.
///
/// The provider's `efforts` becomes the union of what its models advertise, so
/// `model.efforts ⊆ provider.efforts` holds for discovered entries exactly as it
/// does for built-in ones. Without that the client could not place a narrowed
/// model on the provider's scale.
pub(crate) async fn provider_for(
    state: &crate::neev::startup::ServerState,
    actor: &crate::neev::types::OrgContext,
    provider: &'static str,
) -> ProviderCapabilities {
    let mut capabilities = provider_capabilities(provider);
    let discovery = super::discovery::catalogue(state, actor, provider).await;
    if discovery.models.is_empty() {
        capabilities.discovery_unavailable_reason = discovery.unavailable.map(str::to_owned);
        return capabilities;
    }
    let mut union: Vec<String> = Vec::new();
    capabilities.models = discovery
        .models
        .iter()
        .map(|model| {
            if let Some(efforts) = &model.efforts {
                for effort in efforts {
                    if !union.contains(effort) {
                        union.push(effort.clone());
                    }
                }
            }
            ModelDto {
                id: model.id.clone(),
                label: model.label.clone(),
                default: model.default,
                supports_speed: model.supports_speed,
                efforts: model.efforts.clone(),
                default_effort: model.default_effort.clone(),
            }
        })
        .collect();
    // A discovered catalogue that named no efforts at all leaves the built-in
    // scale in place rather than an empty one, which would read as "this
    // provider has no effort control".
    if !union.is_empty() {
        capabilities.efforts = union;
    }
    capabilities
}

/// What a (provider, model) pair actually allows for THIS caller.
///
/// Discovery first, built-ins second. The levels are open strings because the
/// provider says so — its own schema types one as "a non-empty reasoning effort
/// value advertised by the model" — so this returns the set rather than an enum
/// nothing outside this file could extend.
pub(crate) struct Allowed {
    pub(crate) efforts: Vec<String>,
    pub(crate) supports_speed: bool,
    /// Whether the model appeared in a catalogue at all. A deployment-pinned id
    /// that did not is still runnable; it simply has nothing declared about it.
    pub(crate) catalogued: bool,
}

pub(crate) async fn allowed(
    state: &crate::neev::startup::ServerState,
    actor: &crate::neev::types::OrgContext,
    provider: &str,
    model: &str,
) -> Allowed {
    let discovery = match provider {
        "codex" => Some(super::discovery::catalogue(state, actor, provider).await),
        _ => None,
    };
    if let Some(discovery) = discovery {
        if let Some(found) = discovery.models.iter().find(|entry| entry.id == model) {
            let efforts = match &found.efforts {
                // The model named its own set: that set is the authority, empty
                // included — an empty list means it supports none.
                Some(list) => list.clone(),
                // It named nothing, so the provider's scale applies.
                None => static_efforts(provider),
            };
            return Allowed {
                efforts,
                supports_speed: found.supports_speed,
                catalogued: true,
            };
        }
        if !discovery.models.is_empty() {
            // A real catalogue that does not contain this id: nothing is
            // declared about it, so nothing extra is permitted.
            return Allowed {
                efforts: static_efforts(provider),
                supports_speed: false,
                catalogued: false,
            };
        }
    }
    let entry = models(provider).iter().find(|entry| entry.id == model);
    Allowed {
        efforts: entry
            .and_then(|entry| entry.efforts)
            .map(|list| list.iter().map(|effort| (*effort).to_owned()).collect())
            .unwrap_or_else(|| static_efforts(provider)),
        supports_speed: entry.is_some_and(|entry| entry.supports_speed),
        catalogued: entry.is_some(),
    }
}

/// A stored effort must be a short, argv-safe, non-empty token. The VOCABULARY
/// is the provider's; this is only the shape, and it is the SAME shape discovery
/// applies and the client parses, so a level cannot be accepted by one layer and
/// refused by another.
pub(crate) fn safe_effort(value: &str) -> bool {
    super::discovery::safe_effort_token(value)
}

/// Validate a whole (model, effort, fast) triple against what this CALLER's
/// catalogue allows.
pub(crate) async fn validated_for(
    state: &crate::neev::startup::ServerState,
    actor: &crate::neev::types::OrgContext,
    provider: &str,
    model: &str,
    effort: Option<&str>,
    fast: bool,
) -> Result<(Option<String>, bool), ()> {
    if !crate::neev::agents::execution::safe_model(model) {
        return Err(());
    }
    let allowed = allowed(state, actor, provider, model).await;
    let level = match effort.filter(|value| !value.is_empty()) {
        None => None,
        Some(value) => {
            if !safe_effort(value) || !allowed.efforts.iter().any(|entry| entry == value) {
                return Err(());
            }
            Some(value.to_owned())
        }
    };
    if fast && !allowed.supports_speed {
        return Err(());
    }
    Ok((level, fast))
}

/// The catalogue the UI renders from.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capabilities {
    pub(crate) providers: Vec<ProviderCapabilities>,
}

pub(crate) fn catalogue() -> Capabilities {
    Capabilities {
        providers: vec![
            provider_capabilities("claude"),
            provider_capabilities("codex"),
        ],
    }
}

/// The catalogue as THIS caller should see it, with their own account's models.
pub(crate) async fn catalogue_for(
    state: &crate::neev::startup::ServerState,
    actor: &crate::neev::types::OrgContext,
) -> Capabilities {
    Capabilities {
        providers: vec![
            provider_for(state, actor, "claude").await,
            provider_for(state, actor, "codex").await,
        ],
    }
}

/// Accept an effort for this provider, or refuse it.
///
/// `None` in means "unset", which is always allowed and sends no flag. A value
/// this provider cannot honour is refused HERE, because the CLI would accept
/// the argument and quietly run at its default instead.
pub(crate) fn validated_effort_for(
    provider: &str,
    model: &str,
    effort: Option<&str>,
) -> Result<Option<Effort>, ()> {
    let level = validated_effort(provider, effort)?;
    // A model may accept fewer levels than its provider advertises. Checking
    // only the provider's list was safe while every entry declared none, and
    // stops being safe the moment discovery fills them in — which is exactly
    // when nobody would be looking at this function again.
    if let (Some(level), Some(entry)) = (
        level,
        models(provider).iter().find(|entry| entry.id == model),
    ) {
        if let Some(allowed) = entry.efforts {
            if !allowed.contains(&level.name()) {
                return Err(());
            }
        }
    }
    Ok(level)
}

pub(crate) fn validated_effort(provider: &str, effort: Option<&str>) -> Result<Option<Effort>, ()> {
    let Some(value) = effort else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = Effort::parse(value).ok_or(())?;
    if !parsed.supported_by(provider) {
        return Err(());
    }
    Ok(Some(parsed))
}

/// Accept a whole (model, effort, fast) triple, or refuse it.
///
/// Combined on purpose. Validating the three separately lets a caller pass
/// through an invalid intermediate — Haiku with Fast still on from the previous
/// model — and makes three revisions out of one decision, which two concurrent
/// editors can interleave. One check, one answer.
pub(crate) fn validated_settings(
    provider: &str,
    model: &str,
    effort: Option<&str>,
    fast: bool,
) -> Result<(Option<Effort>, bool), ()> {
    if !crate::neev::agents::execution::safe_model(model) {
        return Err(());
    }
    let level = validated_effort_for(provider, model, effort)?;
    let fast = validated_speed(provider, model, fast)?;
    Ok((level, fast))
}

/// Accept a speed choice for this provider and model, or refuse it.
///
/// Refused rather than silently switching the model, which is the tempting
/// shortcut: a reader who picked Haiku and Fast asked for two things, and
/// quietly giving them Opus would be answering a question they did not ask with
/// somebody else's money.
pub(crate) fn validated_speed(provider: &str, model: &str, fast: bool) -> Result<bool, ()> {
    if !fast {
        return Ok(false);
    }
    let entry = models(provider).iter().find(|entry| entry.id == model);
    match entry {
        Some(entry) if entry.supports_speed => Ok(true),
        // A model the catalogue does not name (a deployment-pinned full name)
        // has no established Fast support, so Fast is refused for it.
        _ => Err(()),
    }
}

/// Whether a model id is one this provider is known to take.
///
/// A catalogue entry is known good. Anything else is allowed only if it is
/// argv-safe, which is how a deployment pins a full model name the catalogue
/// cannot enumerate — the CLI is then the one that decides, and its refusal
/// surfaces as a failed turn rather than as a wrong-model success.
pub(crate) fn known_model(provider: &str, model: &str) -> bool {
    models(provider).iter().any(|entry| entry.id == model)
}

#[cfg(test)]
#[path = "capabilities_tests.rs"]
mod capabilities_tests;
