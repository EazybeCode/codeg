#![allow(dead_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;
use std::fmt;

const COUNTER_MAX: u64 = i64::MAX as u64;
const NATIVE_ID_MAX: u64 = i32::MAX as u64;
const UNIX_TIME_MAX: u64 = 253_402_300_799;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Begin,
    Issue,
    Complete,
    Session,
    Redeem,
    Validate,
    Logout,
    Revoke,
    Revocations,
    Ack,
}

impl Operation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Issue => "issue",
            Self::Complete => "complete",
            Self::Session => "session",
            Self::Redeem => "redeem",
            Self::Validate => "validate",
            Self::Logout => "logout",
            Self::Revoke => "revoke",
            Self::Revocations => "revocations",
            Self::Ack => "ack",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Version;

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(1)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserialize_bounded_number(deserializer, 1, 1)? == 1 {
            Ok(Self)
        } else {
            Err(D::Error::custom("invalid number"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnixTime(u64);

impl UnixTime {
    pub(crate) fn new(value: u64) -> Result<Self, ()> {
        (value <= UNIX_TIME_MAX).then_some(Self(value)).ok_or(())
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl Serialize for UnixTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for UnixTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(deserialize_bounded_number(deserializer, 0, UNIX_TIME_MAX)?)
            .map_err(|()| D::Error::custom("invalid time"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limit(u8);

impl Limit {
    pub(crate) fn new(value: u64) -> Result<Self, ()> {
        if (1..=50).contains(&value) {
            Ok(Self(value as u8))
        } else {
            Err(())
        }
    }

    pub(crate) fn get(self) -> u8 {
        self.0
    }
}

impl Serialize for Limit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Limit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(deserialize_bounded_number(deserializer, 1, 50)?)
            .map_err(|()| D::Error::custom("invalid limit"))
    }
}

fn deserialize_bounded_number<'de, D: Deserializer<'de>>(
    deserializer: D,
    minimum: u64,
    maximum: u64,
) -> Result<u64, D::Error> {
    struct BoundedNumberVisitor {
        minimum: u64,
        maximum: u64,
    }

    impl<'de> Visitor<'de> for BoundedNumberVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded integer-valued JSON number")
        }

        fn visit_u64<E: DeError>(self, value: u64) -> Result<Self::Value, E> {
            if (self.minimum..=self.maximum).contains(&value) {
                Ok(value)
            } else {
                Err(E::custom("number out of range"))
            }
        }

        fn visit_i64<E: DeError>(self, value: i64) -> Result<Self::Value, E> {
            let value = u64::try_from(value).map_err(|_| E::custom("number out of range"))?;
            self.visit_u64(value)
        }

        fn visit_f64<E: DeError>(self, value: f64) -> Result<Self::Value, E> {
            if !value.is_finite()
                || value.fract() != 0.0
                || value < self.minimum as f64
                || value > self.maximum as f64
            {
                return Err(E::custom("number is not a bounded integer"));
            }
            Ok(value as u64)
        }
    }

    deserializer.deserialize_any(BoundedNumberVisitor { minimum, maximum })
}

macro_rules! string_type {
    ($name:ident, $validator:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub(crate) struct $name(String);

        impl $name {
            pub(crate) fn new(value: String) -> Result<Self, ()> {
                if $validator(&value) {
                    Ok(Self(value))
                } else {
                    Err(())
                }
            }

            pub(crate) fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(|()| D::Error::custom("invalid string"))
            }
        }
    };
}

fn valid_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
}

fn valid_token(value: &str) -> bool {
    if value.len() != 43
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return false;
    }
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };
    decoded.len() == 32 && URL_SAFE_NO_PAD.encode(decoded) == value
}

fn canonical_decimal(value: &str, allow_zero: bool, maximum: u64) -> bool {
    if value.is_empty()
        || value.len() > 19
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (!allow_zero && value == "0")
        || (value.len() > 1 && value.starts_with('0'))
    {
        return false;
    }
    value.parse::<u64>().is_ok_and(|parsed| parsed <= maximum)
}

fn valid_counter(value: &str) -> bool {
    canonical_decimal(value, false, COUNTER_MAX)
}

fn valid_cursor(value: &str) -> bool {
    canonical_decimal(value, true, COUNTER_MAX)
}

fn valid_native_id(value: &str) -> bool {
    canonical_decimal(value, false, NATIVE_ID_MAX)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|character| !matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f))
}

fn valid_email(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 254
        || value.contains("..")
        || value.starts_with('.')
        || value
            .chars()
            .any(|character| matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f))
    {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || domain.contains('@')
        || !local.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'' | b'+' | b'-' | b'.')
        })
        || !local
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-'))
    {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let top = labels.last().copied().unwrap_or_default();
    if top.len() < 2 || !top.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return false;
    }
    labels[..labels.len() - 1].iter().all(|label| {
        label
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

string_type!(UuidV4, valid_uuid_v4);
string_type!(Token, valid_token);
string_type!(Counter, valid_counter);
string_type!(Cursor, valid_cursor);
string_type!(NativeId, valid_native_id);
string_type!(Name, valid_name);
string_type!(Email, valid_email);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct RequiredNullable<T>(Option<T>);

impl<T> RequiredNullable<T> {
    pub(crate) fn new(value: Option<T>) -> Self {
        Self(value)
    }

    fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

fn deserialize_required_nullable<'de, D, T>(
    deserializer: D,
) -> Result<RequiredNullable<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(RequiredNullable)
}

impl Counter {
    fn value(&self) -> u64 {
        self.0.parse().expect("validated counter")
    }
}

impl Cursor {
    fn value(&self) -> u64 {
        self.0.parse().expect("validated cursor")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Role {
    #[serde(rename = "admin")]
    Admin,
    #[serde(rename = "member")]
    Member,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum ErrorCode {
    #[serde(rename = "invalid_request")]
    InvalidRequest,
    #[serde(rename = "unauthorized")]
    Unauthorized,
    #[serde(rename = "forbidden")]
    Forbidden,
    #[serde(rename = "authority_changed")]
    AuthorityChanged,
    #[serde(rename = "login_retry_required")]
    LoginRetryRequired,
    #[serde(rename = "conflict")]
    Conflict,
    #[serde(rename = "rate_limited")]
    RateLimited,
    #[serde(rename = "unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Propagation {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "complete")]
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum EventScope {
    #[serde(rename = "session")]
    Session,
    #[serde(rename = "account")]
    Account,
    #[serde(rename = "org")]
    Org,
    #[serde(rename = "membership")]
    Membership,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Destination {
    #[serde(rename = "/workspace")]
    Workspace,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Issuer {
    #[serde(rename = "neev-native-v1")]
    NeevNativeV1,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Audience {
    #[serde(rename = "neev-collaboration-v1")]
    NeevCollaborationV1,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum LogoutState {
    #[serde(rename = "local_denied")]
    LocalDenied,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum RevokeState {
    #[serde(rename = "revoked")]
    Revoked,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope<D> {
    pub(super) v: Version,
    pub(crate) request_id: UuidV4,
    pub(crate) data: D,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErrorEnvelope {
    pub(super) v: Version,
    pub(crate) request_id: UuidV4,
    pub(crate) error: ErrorBody,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ErrorBody {
    pub(crate) code: ErrorCode,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseEnvelope<D> {
    Success(Envelope<D>),
    Error(ErrorEnvelope),
}

impl<D> ResponseEnvelope<D> {
    fn request_id(&self) -> &str {
        match self {
            Self::Success(envelope) => envelope.request_id.as_str(),
            Self::Error(envelope) => envelope.request_id.as_str(),
        }
    }

    fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Success(_) => None,
            Self::Error(envelope) => Some(envelope.error.code),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyData {}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueRequestData {
    pub(crate) attempt_id: Token,
    pub(crate) challenge: Token,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteRequestData {
    pub(crate) attempt_id: Token,
    pub(crate) code: Token,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedeemRequestData {
    pub(crate) attempt_id: Token,
    pub(crate) code: Token,
    pub(crate) verifier: Token,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidateRequestData {
    pub(crate) relying_ref: UuidV4,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokeRequestData {
    pub(crate) relying_ref: UuidV4,
    pub(crate) intent_id: UuidV4,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationsRequestData {
    pub(crate) after_cursor: Cursor,
    pub(crate) limit: Limit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AckRequestData {
    pub(crate) through_cursor: Cursor,
    pub(crate) writer_epoch: Counter,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BeginSuccessData {
    pub(crate) attempt_id: Token,
    pub(crate) challenge: Token,
    pub(crate) csrf: Token,
    pub(crate) expires_at: UnixTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueSuccessData {
    pub(crate) code: Token,
    pub(crate) expires_at: UnixTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteSuccessData {
    pub(crate) workspace_id: UuidV4,
    pub(crate) user_id: UuidV4,
    pub(crate) destination: Destination,
    pub(crate) csrf: Token,
    pub(crate) csrf_expires_at: UnixTime,
    pub(crate) expires_at: UnixTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionSuccessData {
    pub(crate) workspace_id: UuidV4,
    pub(crate) user_id: UuidV4,
    pub(crate) csrf: Token,
    pub(crate) csrf_expires_at: UnixTime,
    pub(crate) expires_at: UnixTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Subject {
    pub(crate) user_id: NativeId,
    pub(crate) org_id: NativeId,
    pub(crate) source_session_id: UuidV4,
    pub(crate) account_revision: Counter,
    pub(crate) org_revision: Counter,
    pub(crate) member_revision: Counter,
    pub(crate) role: Role,
    pub(crate) display_name: Name,
    pub(crate) org_name: Name,
    pub(crate) verified_email: Email,
}

impl Subject {
    pub(crate) fn role(&self) -> Role {
        self.role
    }

    pub(crate) fn user_id(&self) -> &str {
        self.user_id.as_str()
    }

    pub(crate) fn org_id(&self) -> &str {
        self.org_id.as_str()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedeemSuccessData {
    pub(crate) receipt_id: UuidV4,
    pub(crate) issuer: Issuer,
    pub(crate) audience: Audience,
    pub(crate) relying_ref: UuidV4,
    pub(crate) subject: Subject,
    pub(crate) expires_at: UnixTime,
    pub(crate) issued_after_cursor: Cursor,
    pub(crate) revocation_cursor: Cursor,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidateSuccessData {
    pub(crate) subject: Subject,
    pub(crate) expires_at: UnixTime,
    pub(crate) issued_after_cursor: Cursor,
    pub(crate) revocation_cursor: Cursor,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LogoutSuccessData {
    pub(crate) state: LogoutState,
    pub(crate) propagation: Propagation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokeSuccessData {
    pub(crate) event_id: UuidV4,
    pub(crate) cursor: Counter,
    pub(crate) state: RevokeState,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationEvent {
    pub(crate) event_id: UuidV4,
    pub(crate) cursor: Counter,
    pub(crate) scope: EventScope,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) user_id: RequiredNullable<NativeId>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) org_id: RequiredNullable<NativeId>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) source_session_id: RequiredNullable<UuidV4>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevocationsSuccessData {
    pub(crate) events: Vec<RevocationEvent>,
    pub(crate) next_cursor: Cursor,
    pub(crate) high_watermark: Cursor,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AckSuccessData {
    pub(crate) acknowledged_cursor: Cursor,
}

pub(super) trait Validated {
    fn valid(&self) -> bool;
}

macro_rules! always_valid {
    ($($type:ty),+ $(,)?) => {
        $(impl Validated for $type {
            fn valid(&self) -> bool { true }
        })+
    };
}

always_valid!(
    EmptyData,
    IssueRequestData,
    CompleteRequestData,
    RedeemRequestData,
    ValidateRequestData,
    RevokeRequestData,
    RevocationsRequestData,
    AckRequestData,
    BeginSuccessData,
    IssueSuccessData,
    Subject,
    LogoutSuccessData,
    RevokeSuccessData,
    AckSuccessData,
);

impl Validated for CompleteSuccessData {
    fn valid(&self) -> bool {
        self.csrf_expires_at.get() <= self.expires_at.get()
    }
}

impl Validated for SessionSuccessData {
    fn valid(&self) -> bool {
        self.csrf_expires_at.get() <= self.expires_at.get()
    }
}

impl Validated for RedeemSuccessData {
    fn valid(&self) -> bool {
        self.issued_after_cursor.value() <= self.revocation_cursor.value()
            && self.relying_ref != self.subject.source_session_id
    }
}

impl Validated for ValidateSuccessData {
    fn valid(&self) -> bool {
        self.issued_after_cursor.value() <= self.revocation_cursor.value()
    }
}

impl RevocationEvent {
    fn valid_scope(&self) -> bool {
        let user = self.user_id.is_some();
        let org = self.org_id.is_some();
        let session = self.source_session_id.is_some();
        match self.scope {
            EventScope::Session => user && org && session,
            EventScope::Account => user && !org && !session,
            EventScope::Org => !user && org && !session,
            EventScope::Membership => user && org && !session,
        }
    }
}

impl Validated for RevocationsSuccessData {
    fn valid(&self) -> bool {
        if self.events.len() > 50 || self.next_cursor.value() > self.high_watermark.value() {
            return false;
        }
        if self
            .events
            .last()
            .is_some_and(|event| event.cursor.as_str() != self.next_cursor.as_str())
        {
            return false;
        }
        let mut ids = HashSet::with_capacity(self.events.len());
        self.events.iter().enumerate().all(|(index, event)| {
            event.valid_scope()
                && ids.insert(event.event_id.as_str())
                && (index == 0
                    || event.cursor.value()
                        == self.events[index - 1].cursor.value().saturating_add(1))
        })
    }
}

impl<D: Validated> Validated for Envelope<D> {
    fn valid(&self) -> bool {
        self.data.valid()
    }
}

impl Validated for ErrorEnvelope {
    fn valid(&self) -> bool {
        true
    }
}

impl<D: Validated> Validated for ResponseEnvelope<D> {
    fn valid(&self) -> bool {
        match self {
            Self::Success(envelope) => envelope.valid(),
            Self::Error(envelope) => envelope.valid(),
        }
    }
}

pub(super) type BeginRequest = Envelope<EmptyData>;
pub(super) type IssueRequest = Envelope<IssueRequestData>;
pub(super) type CompleteRequest = Envelope<CompleteRequestData>;
pub(super) type SessionRequest = Envelope<EmptyData>;
pub(super) type RedeemRequest = Envelope<RedeemRequestData>;
pub(super) type ValidateRequest = Envelope<ValidateRequestData>;
pub(super) type LogoutRequest = Envelope<EmptyData>;
pub(super) type RevokeRequest = Envelope<RevokeRequestData>;
pub(super) type RevocationsRequest = Envelope<RevocationsRequestData>;
pub(super) type AckRequest = Envelope<AckRequestData>;

pub(super) type BeginResponse = ResponseEnvelope<BeginSuccessData>;
pub(super) type IssueResponse = ResponseEnvelope<IssueSuccessData>;
pub(super) type CompleteResponse = ResponseEnvelope<CompleteSuccessData>;
pub(super) type SessionResponse = ResponseEnvelope<SessionSuccessData>;
pub(super) type RedeemResponse = ResponseEnvelope<RedeemSuccessData>;
pub(super) type ValidateResponse = ResponseEnvelope<ValidateSuccessData>;
pub(super) type LogoutResponse = ResponseEnvelope<LogoutSuccessData>;
pub(super) type RevokeResponse = ResponseEnvelope<RevokeSuccessData>;
pub(super) type RevocationsResponse = ResponseEnvelope<RevocationsSuccessData>;
pub(super) type AckResponse = ResponseEnvelope<AckSuccessData>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Begin(BeginRequest),
    Issue(IssueRequest),
    Complete(CompleteRequest),
    Session(SessionRequest),
    Redeem(RedeemRequest),
    Validate(ValidateRequest),
    Logout(LogoutRequest),
    Revoke(RevokeRequest),
    Revocations(RevocationsRequest),
    Ack(AckRequest),
}

impl Request {
    pub(crate) fn operation(&self) -> Operation {
        match self {
            Self::Begin(_) => Operation::Begin,
            Self::Issue(_) => Operation::Issue,
            Self::Complete(_) => Operation::Complete,
            Self::Session(_) => Operation::Session,
            Self::Redeem(_) => Operation::Redeem,
            Self::Validate(_) => Operation::Validate,
            Self::Logout(_) => Operation::Logout,
            Self::Revoke(_) => Operation::Revoke,
            Self::Revocations(_) => Operation::Revocations,
            Self::Ack(_) => Operation::Ack,
        }
    }

    pub(crate) fn request_id(&self) -> &str {
        match self {
            Self::Begin(value) => value.request_id.as_str(),
            Self::Issue(value) => value.request_id.as_str(),
            Self::Complete(value) => value.request_id.as_str(),
            Self::Session(value) => value.request_id.as_str(),
            Self::Redeem(value) => value.request_id.as_str(),
            Self::Validate(value) => value.request_id.as_str(),
            Self::Logout(value) => value.request_id.as_str(),
            Self::Revoke(value) => value.request_id.as_str(),
            Self::Revocations(value) => value.request_id.as_str(),
            Self::Ack(value) => value.request_id.as_str(),
        }
    }

    pub(super) fn valid(&self) -> bool {
        match self {
            Self::Begin(value) => value.valid(),
            Self::Issue(value) => value.valid(),
            Self::Complete(value) => value.valid(),
            Self::Session(value) => value.valid(),
            Self::Redeem(value) => value.valid(),
            Self::Validate(value) => value.valid(),
            Self::Logout(value) => value.valid(),
            Self::Revoke(value) => value.valid(),
            Self::Revocations(value) => value.valid(),
            Self::Ack(value) => value.valid(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Response {
    Begin(BeginResponse),
    Issue(IssueResponse),
    Complete(CompleteResponse),
    Session(SessionResponse),
    Redeem(RedeemResponse),
    Validate(ValidateResponse),
    Logout(LogoutResponse),
    Revoke(RevokeResponse),
    Revocations(RevocationsResponse),
    Ack(AckResponse),
}

impl Response {
    pub(crate) fn operation(&self) -> Operation {
        match self {
            Self::Begin(_) => Operation::Begin,
            Self::Issue(_) => Operation::Issue,
            Self::Complete(_) => Operation::Complete,
            Self::Session(_) => Operation::Session,
            Self::Redeem(_) => Operation::Redeem,
            Self::Validate(_) => Operation::Validate,
            Self::Logout(_) => Operation::Logout,
            Self::Revoke(_) => Operation::Revoke,
            Self::Revocations(_) => Operation::Revocations,
            Self::Ack(_) => Operation::Ack,
        }
    }

    pub(crate) fn request_id(&self) -> &str {
        match self {
            Self::Begin(value) => value.request_id(),
            Self::Issue(value) => value.request_id(),
            Self::Complete(value) => value.request_id(),
            Self::Session(value) => value.request_id(),
            Self::Redeem(value) => value.request_id(),
            Self::Validate(value) => value.request_id(),
            Self::Logout(value) => value.request_id(),
            Self::Revoke(value) => value.request_id(),
            Self::Revocations(value) => value.request_id(),
            Self::Ack(value) => value.request_id(),
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        self.error_code().is_some()
    }

    pub(crate) fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Begin(value) => value.error_code(),
            Self::Issue(value) => value.error_code(),
            Self::Complete(value) => value.error_code(),
            Self::Session(value) => value.error_code(),
            Self::Redeem(value) => value.error_code(),
            Self::Validate(value) => value.error_code(),
            Self::Logout(value) => value.error_code(),
            Self::Revoke(value) => value.error_code(),
            Self::Revocations(value) => value.error_code(),
            Self::Ack(value) => value.error_code(),
        }
    }

    pub(crate) fn subject(&self) -> Option<&Subject> {
        match self {
            Self::Redeem(ResponseEnvelope::Success(envelope)) => Some(&envelope.data.subject),
            Self::Validate(ResponseEnvelope::Success(envelope)) => Some(&envelope.data.subject),
            _ => None,
        }
    }

    pub(crate) fn revocation_cursor(&self) -> Option<&str> {
        match self {
            Self::Redeem(ResponseEnvelope::Success(envelope)) => {
                Some(envelope.data.revocation_cursor.as_str())
            }
            Self::Validate(ResponseEnvelope::Success(envelope)) => {
                Some(envelope.data.revocation_cursor.as_str())
            }
            _ => None,
        }
    }

    pub(super) fn valid(&self) -> bool {
        match self {
            Self::Begin(value) => value.valid(),
            Self::Issue(value) => value.valid(),
            Self::Complete(value) => value.valid(),
            Self::Session(value) => value.valid(),
            Self::Redeem(value) => value.valid(),
            Self::Validate(value) => value.valid(),
            Self::Logout(value) => value.valid(),
            Self::Revoke(value) => value.valid(),
            Self::Revocations(value) => value.valid(),
            Self::Ack(value) => value.valid(),
        }
    }
}
