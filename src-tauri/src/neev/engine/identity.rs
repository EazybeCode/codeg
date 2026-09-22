//! The identity the proxy asserts to Docs, and the headers that carry it.
//!
//! Docs authenticates with Django's `PersistentRemoteUserMiddleware` behind this
//! proxy. That is a real authentication mechanism *provided* the origin cannot
//! be reached without the proxy and the proxy cannot be told who the caller is
//! by the caller. Both conditions live here:
//!
//! - [`STRIPPED`] is removed from every client request before anything reads it,
//!   so a forged header is indistinguishable from an absent one;
//! - the values in [`Assertion`] are derived from the Neev session the server
//!   already authenticated, never from request headers.
//!
//! Why headers at all, when the collaboration socket cannot use them: upstream's
//! y-provider forwards only `cookie`, `origin` and its own API key to the Django
//! backend, so the header establishes the Django session on the HTTP path and
//! the resulting `docs_sessionid` is what authorizes the socket. That is also
//! why the middleware must be the *persistent* variant — the plain one logs the
//! session out whenever the header is absent, which is every y-provider call.

use crate::neev::types::OrgContext;

/// Carries the authenticated user to Docs. `REMOTE_USER` in Django's WSGI
/// environ; the rest populate the user record on first sight.
pub(crate) const USER: &str = "x-neev-remote-user";
pub(crate) const EMAIL: &str = "x-neev-remote-email";
pub(crate) const NAME: &str = "x-neev-remote-name";

/// Headers deleted from every inbound client request before it is classified,
/// authorized or forwarded.
///
/// `x-forwarded-proto` is in this list for a reason that is easy to miss:
/// upstream's `Production` settings set
/// `SECURE_PROXY_SSL_HEADER = ("HTTP_X_FORWARDED_PROTO", "https")`, so Django
/// trusts it to decide whether the request was secure. Left to the client it
/// would be a way to talk Django out of its own TLS assumptions. The proxy sets
/// it itself after stripping.
///
/// `x-y-provider-key` is stripped because it is y-provider's shared secret with
/// the backend; a client must never be able to present it.
pub(crate) const STRIPPED: &[&str] = &[
    USER,
    EMAIL,
    NAME,
    "remote-user",
    "remote_user",
    "x-remote-user",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-forwarded-for",
    "x-forwarded-port",
    "x-real-ip",
    "x-y-provider-key",
    // The bridge's private credential. A client presenting it would reach the
    // provisioning and access-projection API directly.
    super::upstream::BRIDGE_KEY,
];

/// What the proxy asserts about the caller on a forwarded request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assertion {
    /// Becomes Docs' `User.sub`.
    pub(crate) subject: String,
}

/// Derive the Docs subject for a Neev actor.
///
/// Two properties matter and neither is cosmetic.
///
/// **Stable.** It is the Neev user id, which never changes for an account, so a
/// user keeps one Docs identity across sessions, renames and email changes.
/// Deriving it from the email instead would hand a user a different Docs
/// identity — and so a different set of document accesses — the moment they
/// changed their address.
///
/// **Organization-scoped.** Neev users exist inside an organization and Neev's
/// own authorization is always evaluated as `(org_id, user_id)`. Without the
/// organization in the subject, one Neev account that belongs to two
/// organizations would present a single Docs identity across both, and any Docs
/// access row it accumulated in one would be carried into the other. Neev would
/// still refuse the request at the proxy, so this is defence in depth rather
/// than the only barrier — but it is the difference between a bug being
/// contained and being a cross-tenant leak.
pub(crate) fn subject(actor: &OrgContext) -> String {
    format!("neev:{}:{}", actor.org_id, actor.identity.user_id)
}

/// The grammar the bridge validates subjects against: `neev:<org>:<user>`, each
/// segment `[0-9A-Za-z_-]{1,64}`, ASCII, total at most 255. Asserted here so a
/// change to [`subject`] that the bridge would reject fails in this repository's
/// tests rather than at runtime against Docs.
pub(crate) fn well_formed(subject: &str) -> bool {
    if subject.len() > 255 || !subject.is_ascii() {
        return false;
    }
    let segments: Vec<&str> = subject.split(':').collect();
    segments.len() == 3
        && segments[0] == "neev"
        && segments[1..].iter().all(|segment| {
            (1..=64).contains(&segment.len())
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// Whether a header is one the client is never allowed to supply.
pub(crate) fn stripped(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    STRIPPED.contains(&name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neev::types::{IdentityContext, Role};

    fn actor(org_id: i32, user_id: i32) -> OrgContext {
        OrgContext {
            identity: IdentityContext {
                user_id,
                session_id: "session".into(),
                auth_revision: 1,
                authenticated_at: 0,
            },
            org_id,
            member_id: 1,
            member_revision: 1,
            org_revision: 1,
            role: Role::Member,
        }
    }

    #[test]
    fn the_subject_is_stable_for_an_account_and_scoped_to_its_organization() {
        // Same account, same organization, different session: one Docs identity.
        let mut later = actor(7, 42);
        later.identity.session_id = "another-session".into();
        later.identity.auth_revision = 9;
        assert_eq!(subject(&actor(7, 42)), subject(&later));

        // Same account in a second organization is a different Docs identity, so
        // a Docs access row accrued in one cannot be carried into the other.
        assert_ne!(subject(&actor(7, 42)), subject(&actor(8, 42)));
        // Different accounts never collide.
        assert_ne!(subject(&actor(7, 42)), subject(&actor(7, 43)));
    }

    #[test]
    fn every_subject_matches_the_grammar_the_bridge_validates() {
        // Neev ids are i32, so segments are always ASCII digits (with a possible
        // leading `-`, which the grammar permits). If that ever stops being
        // true, this fails here rather than as an opaque 400 from the bridge.
        for (org, user) in [(1, 1), (i32::MAX, i32::MAX), (7, 42), (1, 23), (12, 3)] {
            let subject = subject(&actor(org, user));
            assert!(well_formed(&subject), "bridge would reject {subject}");
        }
        // The grammar itself rejects what the bridge says it rejects.
        for bad in [
            "neev:1",                       // too few segments
            "neev:1:2:3",                   // too many
            "other:1:2",                    // wrong namespace
            "neev::2",                      // empty segment
            "neev:1:café",                  // non-ASCII
            "neev:1:has space",
        ] {
            assert!(!well_formed(bad), "grammar accepted {bad}");
        }
    }

    #[test]
    fn subjects_cannot_be_made_to_collide_across_the_separator() {
        // Ids are integers, so `neev:1:23` and `neev:12:3` must stay distinct —
        // a subject scheme that let two different (org, user) pairs render the
        // same string would silently merge two identities.
        assert_ne!(subject(&actor(1, 23)), subject(&actor(12, 3)));
        assert_eq!(subject(&actor(1, 23)), "neev:1:23");
    }

    #[test]
    fn every_identity_header_a_client_could_forge_is_stripped() {
        for header in [
            USER,
            EMAIL,
            NAME,
            "REMOTE-USER",
            "Remote-User",
            "X-Remote-User",
            "X-Neev-Remote-User",
        ] {
            assert!(stripped(header), "not stripped: {header}");
        }
        // Django trusts this one to decide the request was secure, so it must be
        // the proxy's to set and never the client's to assert.
        assert!(stripped("X-Forwarded-Proto"));
        // y-provider's shared secret with the backend is never a client's to send.
        assert!(stripped("X-Y-Provider-Key"));
        // Nor is the bridge credential, which would otherwise reach the private
        // provisioning and access-projection API directly.
        assert!(stripped("X-Neev-Bridge-Key"));
        // Headers the editor legitimately needs are not stripped: CSRF stays on,
        // so both halves of Django's check must survive the proxy.
        for header in ["cookie", "origin", "x-csrftoken", "content-type", "accept"] {
            assert!(!stripped(header), "wrongly stripped: {header}");
        }
    }
}
