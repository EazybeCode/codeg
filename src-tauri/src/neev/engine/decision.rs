//! The proxy's decision, separated from its plumbing.
//!
//! Everything a request is allowed to do is decided here, from the classified
//! target and Neev's own answer about the resource. Keeping it a pure function
//! is the point: the security properties are then testable without a socket, a
//! database or a Docs instance, and the handler around it has nothing left to
//! get wrong except moving bytes.

use super::routes::{Body, Target};
use super::upstream::Projected;

/// What Neev's own authorization said about the resource this request names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grant {
    /// No access at all — including a resource in another organization, or one
    /// that does not exist. The two are deliberately indistinguishable.
    None,
    /// Readable but not writable: an archived resource, in today's native model.
    Read,
    /// Readable and writable.
    Write,
}

/// Why a request was refused. Kept distinct from the outcome so the handler can
/// answer with the right status without re-deriving anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// There is no live Neev session.
    Unauthenticated,
    /// The route is not on the allowlist, or the method is not allowed on it.
    NotAllowed,
    /// The caller cannot see this resource. Answered as not-found rather than
    /// forbidden, so the proxy does not confirm that an id exists to someone who
    /// may not know.
    Unknown,
    /// The caller can read but tried to write.
    ReadOnly,
    /// The body carried a field this route may not change.
    Body,
    /// The resource is not backed by the engine, so the engine must not serve
    /// it. A legacy document reached through an engine route would render
    /// blank; refusing is the honest answer.
    NotEngineBacked,
}

/// What the handler should do once the decision is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Serve a file from the frontend's static export. Never proxied.
    Asset,
    /// Forward to the Docs backend, asserting the caller's identity.
    ///
    /// `role` is present whenever the request names a document. It must be
    /// projected onto the caller's Docs access row *before* the request is
    /// forwarded: a non-creator has no access row of their own, so upstream
    /// would answer 403 and the editor would never load — the socket is not the
    /// only path that needs the projection.
    Forward {
        resource: Option<String>,
        role: Option<Projected>,
    },
    /// Serve the editor shell for an authorized document.
    ///
    /// The shell is a **static file from the export**, not a backend route:
    /// Django serves nothing under `/docs/`, so forwarding it would 404 every
    /// document while every asset still loaded. It is a separate outcome from
    /// `Asset` because it is authorized against a specific document first.
    Shell { resource: String, role: Projected },
    /// Upgrade the collaboration socket for this document, with the caller
    /// projected at this role.
    Upgrade { resource: String, role: Projected },
    Refuse(Refusal),
}

/// Decide a classified request.
///
/// `authenticated` is whether a live Neev session backs this request;
/// `grant` is `access::authorized`'s answer for the named resource, re-derived
/// for this request and never cached across one; `engine_backed` is whether the
/// resource is bound to this engine.
///
/// Order matters and is not arbitrary. Authentication is checked before
/// anything, so an unauthenticated caller learns nothing about which routes
/// exist. The grant is checked before the body, so a caller who may not see a
/// resource cannot probe its field policy. And `Grant::None` answers
/// `Unknown` rather than a forbidden, so the proxy never confirms that an id is
/// real to someone without access — which is what makes a forged or
/// cross-organization id indistinguishable from an invented one.
pub(crate) fn decide(
    target: Option<&Target>,
    authenticated: bool,
    grant: Grant,
    engine_backed: bool,
    body_keys: &[String],
) -> Decision {
    if !authenticated {
        return Decision::Refuse(Refusal::Unauthenticated);
    }
    let Some(target) = target else {
        return Decision::Refuse(Refusal::NotAllowed);
    };
    match target {
        // An asset is the same bytes for everyone and names no document, so a
        // live session is the whole requirement.
        Target::Asset => Decision::Asset,
        // The signed-in user and the frontend's configuration carry no document.
        Target::Account => Decision::Forward {
            resource: None,
            role: None,
        },
        Target::Editor { resource } => match guard(grant, false, engine_backed) {
            Some(refusal) => Decision::Refuse(refusal),
            None => Decision::Shell {
                resource: resource.clone(),
                role: Projected::from_neev(grant == Grant::Write),
            },
        },
        Target::Media { resource, .. } => match guard(grant, false, engine_backed) {
            Some(refusal) => Decision::Refuse(refusal),
            None => Decision::Forward {
                resource: Some(resource.clone()),
                role: Some(Projected::from_neev(grant == Grant::Write)),
            },
        },
        Target::Document {
            resource,
            write,
            body,
        } => {
            if let Some(refusal) = guard(grant, *write, engine_backed) {
                return Decision::Refuse(refusal);
            }
            if let Some(refusal) = body_allowed(*body, body_keys) {
                return Decision::Refuse(refusal);
            }
            Decision::Forward {
                resource: Some(resource.clone()),
                role: Some(Projected::from_neev(grant == Grant::Write)),
            }
        }
        Target::Collaboration { resource } => {
            // A socket is opened even for a reader — upstream carries the
            // read-only flag on the connection — so the guard asks only for
            // visibility, and the grant then selects the projected role.
            if let Some(refusal) = guard(grant, false, engine_backed) {
                return Decision::Refuse(refusal);
            }
            Decision::Upgrade {
                resource: resource.clone(),
                role: Projected::from_neev(grant == Grant::Write),
            }
        }
    }
}

fn guard(grant: Grant, write: bool, engine_backed: bool) -> Option<Refusal> {
    match grant {
        Grant::None => return Some(Refusal::Unknown),
        Grant::Read if write => return Some(Refusal::ReadOnly),
        _ => {}
    }
    // Checked after the grant, so this never reveals that an id exists to a
    // caller who cannot see it.
    if !engine_backed {
        return Some(Refusal::NotEngineBacked);
    }
    None
}

/// An unknown top-level key is a refusal, never a silently dropped field.
/// Dropping would let a caller believe a sharing change had been applied when it
/// had been stripped — a worse outcome than a clear error.
fn body_allowed(policy: Body, keys: &[String]) -> Option<Refusal> {
    let permitted = policy.permitted();
    if keys.iter().all(|key| permitted.contains(&key.as_str())) {
        None
    } else {
        Some(Refusal::Body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "b3b1f0d2-8c4a-4f5e-9a1b-2c3d4e5f6a7b";

    fn document(write: bool, body: Body) -> Target {
        Target::Document {
            resource: DOC.into(),
            write,
            body,
        }
    }

    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn an_unauthenticated_request_learns_nothing_about_which_routes_exist() {
        // Same refusal for an allowed route, a denied route and no route at all.
        for target in [
            Some(&Target::Asset),
            Some(&document(false, Body::None)),
            None,
        ] {
            assert_eq!(
                decide(target, false, Grant::Write, true, &[]),
                Decision::Refuse(Refusal::Unauthenticated)
            );
        }
    }

    #[test]
    fn a_resource_the_caller_cannot_see_is_unknown_not_forbidden() {
        // This is what makes a forged id, a cross-organization id and an
        // invented id indistinguishable: none of them confirm existence.
        for target in [
            document(false, Body::None),
            Target::Editor { resource: DOC.into() },
            Target::Collaboration { resource: DOC.into() },
            Target::Media {
                resource: DOC.into(),
                key: format!("{DOC}/attachments/a.png"),
            },
        ] {
            assert_eq!(
                decide(Some(&target), true, Grant::None, true, &[]),
                Decision::Refuse(Refusal::Unknown)
            );
        }
    }

    #[test]
    fn a_reader_may_read_and_may_open_a_socket_but_may_not_write() {
        assert_eq!(
            decide(Some(&document(false, Body::None)), true, Grant::Read, true, &[]),
            Decision::Forward {
                resource: Some(DOC.into()),
                role: Some(Projected::Reader)
            }
        );
        assert_eq!(
            decide(Some(&document(true, Body::TitleOnly)), true, Grant::Read, true, &keys(&["title"])),
            Decision::Refuse(Refusal::ReadOnly)
        );
        // The socket opens, but read-only — upstream carries that on the
        // connection, so a reader is not simply disconnected.
        assert_eq!(
            decide(
                Some(&Target::Collaboration { resource: DOC.into() }),
                true,
                Grant::Read,
                true,
                &[]
            ),
            Decision::Upgrade {
                resource: DOC.into(),
                role: Projected::Reader
            }
        );
        assert_eq!(
            decide(
                Some(&Target::Collaboration { resource: DOC.into() }),
                true,
                Grant::Write,
                true,
                &[]
            ),
            Decision::Upgrade {
                resource: DOC.into(),
                role: Projected::Editor
            }
        );
    }

    #[test]
    fn a_legacy_document_cannot_be_served_through_an_engine_route() {
        // The mirror of the legacy routes refusing engine-backed resources.
        // A legacy document reached here has no Yjs state and would render as a
        // blank document, which looks exactly like data loss.
        for grant in [Grant::Read, Grant::Write] {
            assert_eq!(
                decide(Some(&document(false, Body::None)), true, grant, false, &[]),
                Decision::Refuse(Refusal::NotEngineBacked)
            );
            assert_eq!(
                decide(
                    Some(&Target::Collaboration { resource: DOC.into() }),
                    true,
                    grant,
                    false,
                    &[]
                ),
                Decision::Refuse(Refusal::NotEngineBacked)
            );
        }
        // But it is still refused as *unknown* when the caller cannot see it —
        // the engine check must not become a way to probe for a resource's
        // existence.
        assert_eq!(
            decide(Some(&document(false, Body::None)), true, Grant::None, false, &[]),
            Decision::Refuse(Refusal::Unknown)
        );
    }

    #[test]
    fn a_rename_cannot_smuggle_a_sharing_field() {
        let target = document(true, Body::TitleOnly);
        assert_eq!(
            decide(Some(&target), true, Grant::Write, true, &keys(&["title"])),
            Decision::Forward {
                resource: Some(DOC.into()),
                role: Some(Projected::Editor)
            }
        );
        for smuggled in [
            keys(&["title", "link_reach"]),
            keys(&["link_role"]),
            keys(&["title", "is_favorite"]),
        ] {
            assert_eq!(
                decide(Some(&target), true, Grant::Write, true, &smuggled),
                Decision::Refuse(Refusal::Body),
                "smuggled through: {smuggled:?}"
            );
        }
        // A content save admits content and upstream's websocket flag, and does
        // not become a second way to set a title.
        let content = document(true, Body::Content);
        assert_eq!(
            decide(Some(&content), true, Grant::Write, true, &keys(&["content", "websocket"])),
            Decision::Forward {
                resource: Some(DOC.into()),
                role: Some(Projected::Editor)
            }
        );
        assert_eq!(
            decide(Some(&content), true, Grant::Write, true, &keys(&["content", "title"])),
            Decision::Refuse(Refusal::Body)
        );
    }

    #[test]
    fn assets_and_account_calls_need_a_session_and_nothing_more() {
        assert_eq!(decide(Some(&Target::Asset), true, Grant::None, false, &[]), Decision::Asset);
        assert_eq!(
            decide(Some(&Target::Account), true, Grant::None, false, &[]),
            Decision::Forward { resource: None, role: None }
        );
    }
}
