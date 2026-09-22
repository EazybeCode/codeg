//! Reading a provider's catalogue, and what happens when it cannot be read.
//!
//! The parsing fixtures are the SHAPE the pinned codex app-server actually
//! returns — `result.data`, each entry carrying `supportedReasoningEfforts` as
//! objects — captured from the binary rather than from its JSON schema, whose
//! type names suggested a different field and which a mock would have been
//! happy to confirm.

use super::*;

/// One entry of a real `model/list` answer, trimmed to the fields we read.
const REAL: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"data":[
  {"id":"gpt-6-astra","model":"gpt-6-astra","displayName":"GPT-6-Astra",
   "description":"Our most capable model.","isDefault":true,"hidden":false,
   "supportedReasoningEfforts":[{"reasoningEffort":"low","description":"x"},
                                {"reasoningEffort":"high","description":"y"}],
   "defaultReasoningEffort":"high"},
  {"id":"gpt-5-6-sol","model":"gpt-5-6-sol","displayName":"GPT-5.6-Sol",
   "isDefault":false,"hidden":false,
   "supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"z"}],
   "defaultReasoningEffort":"medium"}
],"nextCursor":null}}"#;

#[test]
fn the_catalogue_is_read_from_result_data_as_the_provider_sends_it() {
    let models = parse_models(REAL);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-6-astra");
    // The display name is what the reader sees; the id is what the CLI gets.
    assert_eq!(models[0].label, "GPT-6-Astra");
    assert!(models[0].default);
    assert_eq!(models[0].efforts.as_deref(), Some(&["low".to_owned(), "high".to_owned()][..]));
    assert_eq!(models[0].default_effort.as_deref(), Some("high"));
    assert!(!models[1].default);
}

#[test]
fn a_hidden_model_is_not_offered() {
    // The provider marks these as not for display. Showing one would offer
    // something it has decided not to.
    let payload = r#"{"result":{"data":[
      {"id":"visible","displayName":"Visible","hidden":false,"isDefault":true},
      {"id":"secret","displayName":"Secret","hidden":true,"isDefault":false}
    ]}}"#;
    let models = parse_models(payload);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "visible");
}

#[test]
fn an_id_that_could_be_read_as_a_flag_never_becomes_a_model() {
    // The catalogue is remote input that ends up in an argv.
    let payload = r#"{"result":{"data":[
      {"id":"--oops","displayName":"Flagged","isDefault":false},
      {"id":"has space","displayName":"Spaced","isDefault":false},
      {"id":"","displayName":"Empty","isDefault":false},
      {"id":"fine-1.2","displayName":"Fine","isDefault":true}
    ]}}"#;
    let models = parse_models(payload);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "fine-1.2");
}

#[test]
fn one_unusable_entry_costs_that_entry_and_not_the_menu() {
    let payload = r#"{"result":{"data":[
      {"nonsense":true},
      {"id":"good","displayName":"Good","isDefault":true}
    ]}}"#;
    assert_eq!(parse_models(payload).len(), 1);
}

#[test]
fn a_model_naming_no_efforts_reports_none_rather_than_pretending() {
    // Empty means "it named none", which the caller renders as "the provider
    // list applies" — not as "no level works".
    let payload = r#"{"result":{"data":[{"id":"plain","displayName":"Plain","isDefault":true}]}}"#;
    let models = parse_models(payload);
    // ABSENT, not empty: the provider said nothing, so the provider-wide list
    // applies. An explicit `[]` would mean it supports none, which is a
    // different answer and must not be conflated.
    assert_eq!(models[0].efforts, None);
    assert_eq!(models[0].default_effort, None);

    let explicit = r#"{"result":{"data":[{"id":"none","displayName":"None","isDefault":true,"supportedReasoningEfforts":[]}]}}"#;
    assert_eq!(
        parse_models(explicit)[0].efforts,
        Some(Vec::new()),
        "an empty list is 'supports none', not 'unspecified'"
    );
}

#[test]
fn a_refusal_or_nonsense_yields_no_catalogue_rather_than_a_wrong_one() {
    // A JSON-RPC error, an unrelated answer, and not-JSON at all.
    assert!(parse_models(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32001,"message":"no"}}"#).is_empty());
    assert!(parse_models(r#"{"result":{"models":[{"id":"wrong-field"}]}}"#).is_empty());
    assert!(parse_models("not json").is_empty());
    assert!(parse_models("").is_empty());
}

#[test]
fn invalidating_drops_one_users_provider_and_leaves_the_rest() {
    let cache = new_cache();
    let entry = |generation: i64| Entry {
        discovery: Discovery {
            models: Vec::new(),
            unavailable: None,
        },
        fetched_at: 0,
    };
    let key = |org: i32, user: i32, provider: &str, generation: i64| Key {
        org_id: org,
        user_id: user,
        provider: provider.to_owned(),
        generation,
    };
    {
        let mut guard = cache.lock().unwrap();
        guard.insert(key(1, 7, "codex", 1), entry(1));
        // Same user, older generation: a credential they have since replaced.
        guard.insert(key(1, 7, "codex", 2), entry(2));
        // A different person, and the same person in a different org.
        guard.insert(key(1, 8, "codex", 1), entry(1));
        guard.insert(key(2, 7, "codex", 1), entry(1));
    }
    invalidate(&cache, 1, 7, "codex");
    let guard = cache.lock().unwrap();
    assert_eq!(guard.len(), 2, "every generation for that user+provider goes");
    assert!(guard.contains_key(&key(1, 8, "codex", 1)));
    assert!(guard.contains_key(&key(2, 7, "codex", 1)));
}

/// A RECORDED `model/list` entry, trimmed to the fields we read and with
/// nothing account-identifying in it.
///
/// The tier fields are the reason this fixture exists: `serviceTiers` carries
/// OBJECTS whose ids are `priority`/`ultrafast`, while the literal `"fast"`
/// lives in `additionalSpeedTiers` as a bare string. Matching `id == "fast"`
/// finds neither, which would have disabled Fast for every Codex model while
/// looking like a working feature.
const RECORDED: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"data":[
  {"id":"gpt-6-astra","model":"gpt-6-astra","displayName":"GPT-6-Astra",
   "isDefault":true,"hidden":false,"defaultServiceTier":null,
   "serviceTiers":[{"id":"priority","name":"Fast","description":"2x speed, increased usage"}],
   "additionalSpeedTiers":["fast"],
   "supportedReasoningEfforts":[{"reasoningEffort":"low","description":"a"},
                                {"reasoningEffort":"medium","description":"b"},
                                {"reasoningEffort":"high","description":"c"}],
   "defaultReasoningEffort":"medium"},
  {"id":"gpt-5.6-sol","model":"gpt-5.6-sol","displayName":"GPT-5.6-Sol",
   "isDefault":false,"hidden":false,"defaultServiceTier":null,
   "serviceTiers":[{"id":"priority","name":"Fast","description":"1.5x speed"},
                   {"id":"ultrafast","name":"Ultrafast","description":"fastest"}],
   "additionalSpeedTiers":["fast"],
   "supportedReasoningEfforts":[{"reasoningEffort":"low","description":"a"}],
   "defaultReasoningEffort":"low"},
  {"id":"no-tiers","displayName":"No Tiers","isDefault":false,"hidden":false,
   "serviceTiers":[],"additionalSpeedTiers":[]}
],"nextCursor":null}}"#;

#[test]
fn the_recorded_tier_shape_reports_fast_where_the_provider_offers_it() {
    let models = parse_models(RECORDED);
    assert_eq!(models.len(), 3);
    // Ids carry dots; the argv guard must not reject them.
    assert_eq!(models[1].id, "gpt-5.6-sol");
    assert!(
        models[0].supports_speed,
        "additionalSpeedTiers names it, and serviceTiers offers a priority tier"
    );
    assert!(models[1].supports_speed);
    assert!(
        !models[2].supports_speed,
        "empty tier lists mean no fast tier, not an unknown one"
    );
    assert_eq!(models[0].default_effort.as_deref(), Some("medium"));
}

#[test]
fn a_malformed_entry_beside_valid_ones_costs_only_itself() {
    // Everything a provider could send that we must not pass on: an unsafe id,
    // a duplicate, a control-character label, an unusable effort token, and an
    // oversized label. The good entries survive.
    let payload = r#"{"result":{"data":[
      {"id":"good-1","displayName":"Good One","isDefault":true,
       "supportedReasoningEfforts":[{"reasoningEffort":"low"},
                                    {"reasoningEffort":"LOUD"},
                                    {"reasoningEffort":"low"},
                                    {"reasoningEffort":"--flag"}]},
      {"id":"--flag","displayName":"Flagged","isDefault":false},
      {"id":"good-1","displayName":"Duplicate","isDefault":false},
      {"id":"ctl","displayName":"bad\u0007label","isDefault":false},
      {"id":"good-2","displayName":"Good Two","isDefault":false}
    ]}}"#;
    let models = parse_models(payload);
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["good-1", "ctl", "good-2"], "only the unusable are dropped");
    // The control character is stripped rather than the entry discarded.
    assert_eq!(models[1].label, "badlabel");
    // Within a model, a bad token costs that token and not the others.
    assert_eq!(
        models[0].efforts.as_deref(),
        Some(&["low".to_owned()][..]),
        "uppercase, duplicate and flag-shaped tokens are dropped individually"
    );
}

#[test]
fn an_effort_token_has_one_shape_across_the_backend() {
    // The same predicate the stored column and the client both rely on.
    for good in ["low", "medium", "high", "xhigh", "max", "ultra-high", "x_2"] {
        assert!(safe_effort_token(good), "{good} should be accepted");
    }
    for bad in ["", "High", "-low", "1st", "has space", "dot.ted", &"x".repeat(33)] {
        assert!(!safe_effort_token(bad), "{bad:?} should be refused");
    }
}
