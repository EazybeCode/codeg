//! Request classification for the document engine — an **allowlist**.
//!
//! Every request under [`PREFIX`] is classified here before anything else
//! happens to it, and anything this module does not recognise is denied. That
//! direction is deliberate: upstream's API is large and grows between releases
//! (`search`, `users`, `favorites`, `trashbin`, `children`, `tree`, `duplicate`,
//! `create-for-owner`, `link-configuration`, `accesses`, `invitations`,
//! `ask-for-access`, `versions`, `ai-*`, `cors-proxy`, … — all verified present
//! at v5.7.0), and a denylist would have to be re-audited against every upstream
//! release while leaking whatever it forgot in the meantime.
//!
//! Three consequences of the allowlist are load-bearing security properties, not
//! side effects:
//!
//! - generic surfaces — document list, document search, user search, sub-document
//!   creation — are unreachable, so no other organization's or user's private
//!   document can be enumerated through them;
//! - upstream's own sharing actions are unreachable, so a Docs-side action cannot
//!   widen a Neev ACL;
//! - every allowed target that touches a document carries that document's id, so
//!   the caller cannot reach one document's data through another's route.
//!
//! The ids named here are **Neev resource ids**. A Neev document resource and its
//! Docs document share one UUIDv4, so there is no mapping to forge: the id is
//! resolved against `(org_id, resource_id)` and a cross-organization or invented
//! id resolves to nothing.

/// Everything the engine owns lives under this single path prefix, which keeps
/// the upstream static export from colliding with Neev's own `/_next`.
pub(crate) const PREFIX: &str = "/document-engine";

/// What a request is allowed to reach, and with what authority. Every variant
/// that names a `resource` must be authorized against Neev before it is served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A file from the frontend's static export. Served by Neev from the build
    /// artifact — never proxied — so it needs a live Neev session but no
    /// per-document decision.
    ///
    /// `path` is the artifact-relative path, already percent-decoded **once**
    /// and traversal-checked after decoding. The export contains literal
    /// bracketed names like `docs/[id]/`, which the browser requests
    /// percent-encoded, so serving the raw request path would miss the file.
    Asset { path: String },
    /// The editor page for one document.
    Editor { resource: String },
    /// A document-scoped backend call. `write` selects the read/write decision
    /// and `body` bounds what the request may change.
    Document {
        resource: String,
        write: bool,
        body: Body,
    },
    /// A backend call carrying no document: the signed-in user and the frontend
    /// configuration. Needs a live Neev session and nothing more.
    Account,
    /// A self-scoped write on the signed-in user, bounded by `body`.
    AccountWrite { body: Body },
    /// The collaboration socket for one document.
    Collaboration { resource: String },
    /// An attachment belonging to one document.
    Media { resource: String, key: String },
}

/// What a forwarded request body is permitted to contain.
///
/// The route allowlist alone is not sufficient for upstream's document detail
/// endpoint: `PATCH /documents/<id>/` accepts the whole `DocumentSerializer`
/// writable field set, so allowing the route in order to rename a document
/// would also allow `link_reach`, `link_role` and the rest to be set — the very
/// sharing controls deliberately left out of the allowlist would be reachable
/// through a field instead of a route. The body is therefore narrowed to the
/// exact keys the embedded editor needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    /// No body is forwarded, and one supplied by the client is refused.
    None,
    /// A JSON object whose only key may be `title`.
    TitleOnly,
    /// The durable Yjs save: `content`, plus upstream's `websocket` flag.
    Content,
    /// A multipart attachment upload, bounded by size rather than by key.
    Attachment,
    /// The signed-in user's own display preference. Self-scoped: it can only
    /// ever change the caller's own Docs user row.
    LanguageOnly,
}

impl Body {
    /// The exact top-level JSON keys this policy admits. An unknown key is a
    /// refusal, not a silently dropped field: dropping would let a caller
    /// believe a sharing change succeeded when it had been stripped.
    pub(crate) fn permitted(self) -> &'static [&'static str] {
        match self {
            Body::None | Body::Attachment => &[],
            Body::TitleOnly => &["title"],
            Body::LanguageOnly => &["language"],
            // `websocket` tells upstream whether the save came from a connected
            // editor; it selects upstream's own read-only guard and is not a
            // permission of ours to grant.
            Body::Content => &["content", "websocket"],
        }
    }
}

/// Path prefixes upstream serves that this proxy never exposes. Listed so they
/// are refused outright rather than quietly treated as static assets.
const RESERVED: &[&str] = &[
    "api/",
    "external_api/",
    "admin/",
    "static/",
    "media/",
    "collaboration/",
    "silk/",
    "_neev/",
];

/// A path segment that could change the meaning of the path after we have
/// classified it. Rejected outright rather than normalised, because normalising
/// and then forwarding the original is how proxies grow confusion bugs.
fn traversable(path: &str) -> bool {
    path.contains("//")
        || path.split('/').any(|segment| segment == "." || segment == "..")
        || path.contains('\\')
        || path.contains('%')
}

/// Canonical lowercase hyphenated UUIDv4, and nothing else. Upstream's
/// y-provider independently requires a v4, and Neev's `access::canonical_id`
/// requires the canonical spelling, so accepting anything looser here would
/// only create a disagreement between the two.
fn resource_id(value: &str) -> Option<String> {
    let parsed = uuid::Uuid::parse_str(value).ok()?;
    if parsed.get_version_num() != 4 || parsed.to_string() != value {
        return None;
    }
    Some(value.to_owned())
}

/// Classify a request. `None` means denied — the default for anything not
/// named below.
///
/// `method` is matched exactly, so a route allowed for `GET` is not thereby
/// allowed for `DELETE`.
pub(crate) fn classify(method: &str, path: &str, query: Option<&str>) -> Option<Target> {
    let rest = path.strip_prefix(PREFIX)?;
    // The prefix must be a whole segment: `/document-engineering/...` is not ours.
    let rest = if rest.is_empty() { "/" } else { rest };
    // Checked on the remainder *before* the leading slash is removed, so
    // `/document-engine//api/...` is still seen as the double slash it is.
    //
    // Structured routes — ids, API paths, the socket — are held to the strict
    // form and refuse percent-encoding outright, because an encoded id is an
    // ambiguity nobody needs and every spelling of one is a chance for two
    // layers to disagree. Static assets cannot be held to that rule: the export
    // genuinely contains `[id]` in filenames, so they are decoded first and
    // checked after. The decision of which rule applies is made below, per
    // branch, rather than once for everything.
    if traversable(rest) && !is_asset_candidate(rest) {
        return None;
    }
    let rest = rest.strip_prefix('/')?;
    let read = matches!(method, "GET" | "HEAD");

    if let Some(api) = rest.strip_prefix("api/v1.0/") {
        return classify_api(method, read, api, query);
    }
    if let Some(key) = rest.strip_prefix("media/") {
        if !read {
            return None;
        }
        // The attachment key begins with the owning document's id, which is what
        // makes an attachment authorizable as that document rather than as a
        // bare object-store key.
        let (document, _) = key.split_once('/')?;
        let resource = resource_id(document)?;
        if !attachment_of(key, &resource) {
            return None;
        }
        return Some(Target::Media {
            resource,
            key: key.to_owned(),
        });
    }
    if let Some(socket) = rest.strip_prefix("collaboration/ws/") {
        if !socket.is_empty() || !read {
            return None;
        }
        // The room is read only to confirm the client asked for the document it
        // claims; the URL actually forwarded upstream is rebuilt from the
        // authorized resource, never from this value.
        let room = query?.strip_prefix("room=")?;
        return Some(Target::Collaboration {
            resource: resource_id(room)?,
        });
    }
    if let Some(page) = rest.strip_prefix("docs/") {
        if !read {
            return None;
        }
        let document = page.strip_suffix('/').unwrap_or(page);
        return Some(Target::Editor {
            resource: resource_id(document)?,
        });
    }
    // Upstream owns these prefixes. Falling through to the asset branch would
    // answer them from the artifact directory — a 404 rather than a breach, but
    // it would also mean a future upstream surface silently became "an asset"
    // instead of being refused, which is the opposite of deny-by-default.
    if RESERVED
        .iter()
        .any(|prefix| rest == prefix.trim_end_matches('/') || rest.starts_with(prefix))
    {
        return None;
    }
    // Anything else under the prefix is a static file from the export. It is
    // read from the artifact directory, so a name that is not in the artifact is
    // simply absent rather than reaching Docs.
    if !read {
        return None;
    }
    // Decoded exactly once, then checked. Checking before decoding would let
    // `%2e%2e%2f` past a traversal test that never saw a `..`; decoding twice
    // would let `%252e%252e` become one.
    let decoded = percent_decode(rest)?;
    if traversable(&decoded) || decoded.starts_with('/') {
        return None;
    }
    Some(Target::Asset { path: decoded })
}

fn classify_api(method: &str, read: bool, api: &str, query: Option<&str>) -> Option<Target> {
    match (api, method) {
        ("config/", "GET" | "HEAD") => return Some(Target::Account),
        ("users/me/", "GET" | "HEAD") => return Some(Target::Account),
        _ => {}
    }
    // The stock editor writes the signed-in user's language at boot, and it
    // does so at `users/<id>/` — its own id, not `me` — and **throws** when the
    // call fails. Denying it therefore did not merely log an error: it broke
    // the editor's boot and left the document body on its loading skeleton.
    //
    // Two things bound it. The body is narrowed to `language`, so it cannot
    // become a general user-object write. And upstream's `UserViewSet` carries
    // `permission_classes = [permissions.IsSelf]`, so an id that is not the
    // caller's own is refused there — a reliance on upstream that is worth
    // naming rather than leaving implicit, since this proxy does not itself
    // know the caller's Docs user id.
    if let Some(rest) = api.strip_prefix("users/") {
        if let Some(id) = rest.strip_suffix('/') {
            if method == "PATCH" && resource_id(id).is_some() {
                return Some(Target::AccountWrite {
                    body: Body::LanguageOnly,
                });
            }
        }
    }
    let _ = read;
    let document = api.strip_prefix("documents/")?;
    let (id, action) = document.split_once('/')?;
    let resource = resource_id(id)?;
    let (write, body) = match (action, method) {
        // Retrieve the document the editor is about to render.
        ("", "GET") => (false, Body::None),
        // The durable Yjs read. The editor blocks on this before it will create
        // the collaboration provider at all — `useCollaboration` returns early
        // until the content query resolves — so refusing it does not degrade
        // the editor, it stops the socket from ever being opened and leaves the
        // body on its loading skeleton.
        //
        // An earlier version of this file asserted `content` was PATCH-only,
        // reading the action decorator and stopping there. The backend answers
        // `allow: GET, PATCH, HEAD, OPTIONS`, measured against the running
        // service. Serving a blank document instead was never an option: it is
        // indistinguishable from content that is gone.
        ("content/", "GET") => (false, Body::None),
        // Rename. Narrowed to `title` because this endpoint would otherwise
        // accept the sharing fields the allowlist exists to withhold.
        ("", "PATCH") => (true, Body::TitleOnly),
        // The durable Yjs save path. This is the one upstream's editor writes
        // and the whole design exists to preserve it.
        ("content/", "PATCH") => (true, Body::Content),
        // Version history, read-only. Supported rather than denied so the
        // editor's History panel is not dead UI, but `versions_detail` also
        // declares DELETE upstream and that is not reachable here: destroying
        // history is not one of the authorities this bridge grants.
        ("versions/", "GET") => (false, Body::None),
        (version, "GET") if version_of(version).is_some() => (false, Body::None),
        // Attachments.
        ("attachment-upload/", "POST") => (true, Body::Attachment),
        ("media-check/", "GET") => {
            // The key is a query parameter, so the route's document id is not by
            // itself a bound on what is asked about. Upstream does check the key
            // against the document's own attachments, but this proxy does not
            // rely on a remote check to keep its own boundary.
            //
            // The real client sends it percent-encoded
            // (`key=<id>%2Fattachments%2F<uuid>.png`), so it is decoded **once**
            // and then checked. Checking first and decoding after would let
            // `%2e%2e%2f` past a traversal check that never saw a `..`.
            let key = query_value(query?, "key")?;
            if !attachment_of(&key, &resource) {
                return None;
            }
            (false, Body::None)
        }
        _ => return None,
    };
    Some(Target::Document {
        resource,
        write,
        body,
    })
}

/// `versions/<version_id>/`, matched against upstream's own declared character
/// set (`[A-Za-z0-9._+\-=~]{1,1024}`) rather than a looser pattern, so a path
/// this proxy admits is one upstream's router would also route.
fn version_of(action: &str) -> Option<&str> {
    let id = action.strip_prefix("versions/")?.strip_suffix('/')?;
    let valid = (1..=1024).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-=~".contains(&b));
    valid.then_some(id)
}

/// Whether a path is a candidate for the static export rather than a structured
/// route. Used only to decide which traversal rule applies; the asset branch
/// still decodes and re-checks before anything is served.
fn is_asset_candidate(rest: &str) -> bool {
    let trimmed = rest.trim_start_matches('/');
    !RESERVED
        .iter()
        .any(|prefix| trimmed == prefix.trim_end_matches('/') || trimmed.starts_with(prefix))
        && !trimmed.starts_with("docs/")
        // A doubled slash or a backslash is never a legitimate asset name, so
        // those stay refused for assets too.
        && !rest.contains("//")
        && !rest.contains('\\')
}

/// One decoded query parameter by name, from anywhere in the query string.
///
/// Requiring the parameter to come first, as an earlier version did, rejected
/// the request the real editor makes.
fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value))?
    })
}

/// Decode `%XX` escapes exactly once. Anything malformed yields `None`, so a
/// truncated or invalid escape is refused rather than guessed at.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = value.get(index + 1..index + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// An attachment key belongs to a document when the document's id is its first
/// segment — the layout upstream's `attachment_upload` writes.
fn attachment_of(key: &str, resource: &str) -> bool {
    !traversable(key)
        && key
            .split_once('/')
            .is_some_and(|(document, rest)| document == resource && !rest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "b3b1f0d2-8c4a-4f5e-9a1b-2c3d4e5f6a7b";
    const OTHER: &str = "11111111-2222-4333-8444-555555555555";

    fn get(path: &str) -> Option<Target> {
        classify("GET", path, None)
    }

    #[test]
    fn the_editor_page_and_its_api_resolve_to_the_document_in_the_path() {
        assert_eq!(
            get(&format!("{PREFIX}/docs/{DOC}/")),
            Some(Target::Editor {
                resource: DOC.into()
            })
        );
        assert_eq!(
            get(&format!("{PREFIX}/api/v1.0/documents/{DOC}/")),
            Some(Target::Document {
                resource: DOC.into(),
                write: false,
                body: Body::None
            })
        );
        assert_eq!(
            classify("PATCH", &format!("{PREFIX}/api/v1.0/documents/{DOC}/content/"), None),
            Some(Target::Document {
                resource: DOC.into(),
                write: true,
                body: Body::Content
            })
        );
    }

    #[test]
    fn renaming_cannot_carry_the_sharing_fields_the_allowlist_withholds() {
        // Upstream's document detail PATCH accepts the whole writable serializer,
        // so allowing the route for renames would otherwise reach link_reach and
        // link_role — the sharing controls deliberately kept off the allowlist.
        let Some(Target::Document { body, write, .. }) =
            classify("PATCH", &format!("{PREFIX}/api/v1.0/documents/{DOC}/"), None)
        else {
            panic!("expected a document target");
        };
        assert!(write);
        assert_eq!(body, Body::TitleOnly);
        assert_eq!(body.permitted(), ["title"]);
        for withheld in ["link_reach", "link_role", "is_favorite", "path", "excerpt"] {
            assert!(
                !body.permitted().contains(&withheld),
                "rename body would admit {withheld}"
            );
        }
        // The durable save admits content and upstream's own websocket flag, and
        // nothing else — a title cannot ride along on a content save either.
        assert_eq!(Body::Content.permitted(), ["content", "websocket"]);
        assert!(!Body::Content.permitted().contains(&"title"));
        assert!(Body::None.permitted().is_empty());
    }

    #[test]
    fn media_check_cannot_ask_about_another_documents_attachment() {
        // The key here is a query parameter, so the route's own id does not bound
        // it. Upstream does check the key against the document, but this proxy
        // does not delegate its boundary to a remote check.
        assert_eq!(
            classify(
                "GET",
                &format!("{PREFIX}/api/v1.0/documents/{DOC}/media-check/"),
                Some(&format!("key={DOC}/attachments/a.png"))
            ),
            Some(Target::Document {
                resource: DOC.into(),
                write: false,
                body: Body::None
            })
        );
        for key in [
            format!("key={OTHER}/attachments/a.png"),
            format!("key={DOC}"),
            format!("key={DOC}/"),
            "key=".into(),
            format!("key=../{OTHER}/attachments/a.png"),
        ] {
            assert_eq!(
                classify(
                    "GET",
                    &format!("{PREFIX}/api/v1.0/documents/{DOC}/media-check/"),
                    Some(&key)
                ),
                None,
                "accepted cross-document key: {key}"
            );
        }
        // And with no key at all it is not a usable route either.
        assert_eq!(
            classify("GET", &format!("{PREFIX}/api/v1.0/documents/{DOC}/media-check/"), None),
            None
        );
    }

    #[test]
    fn reads_and_writes_are_distinguished_by_method_not_by_route() {
        // The same route is a read for GET and a write for PATCH, and a method
        // upstream does not define on it is denied outright rather than being
        // treated as a read.
        let path = format!("{PREFIX}/api/v1.0/documents/{DOC}/");
        assert_eq!(
            classify("GET", &path, None),
            Some(Target::Document {
                resource: DOC.into(),
                write: false,
                body: Body::None
            })
        );
        assert_eq!(
            classify("PATCH", &path, None),
            Some(Target::Document {
                resource: DOC.into(),
                write: true,
                body: Body::TitleOnly
            })
        );
        assert_eq!(classify("DELETE", &path, None), None);
        assert_eq!(classify("POST", &path, None), None);
        // `content` answers GET as well as PATCH — verified against the running
        // backend, which advertises `allow: GET, PATCH, HEAD, OPTIONS`. The
        // editor blocks on this read before creating the provider, so denying
        // it stops the socket from ever opening.
        assert_eq!(
            classify("GET", &format!("{PREFIX}/api/v1.0/documents/{DOC}/content/"), None),
            Some(Target::Document {
                resource: DOC.into(),
                write: false,
                body: Body::None
            })
        );
        // Still not a licence for other methods on it.
        assert_eq!(
            classify("DELETE", &format!("{PREFIX}/api/v1.0/documents/{DOC}/content/"), None),
            None
        );
    }

    #[test]
    fn sharing_and_enumeration_routes_are_not_reachable() {
        // Each of these exists at v5.7.0. None may be reachable: the first three
        // would let a Docs-side action widen a Neev ACL, and the rest would
        // enumerate documents or users outside the caller's Neev grant.
        for path in [
            format!("{PREFIX}/api/v1.0/documents/{DOC}/accesses/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/invitations/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/link-configuration/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/ask-for-access/"),
            format!("{PREFIX}/api/v1.0/documents/"),
            format!("{PREFIX}/api/v1.0/documents/search/"),
            format!("{PREFIX}/api/v1.0/documents/favorites/"),
            format!("{PREFIX}/api/v1.0/documents/trashbin/"),
            format!("{PREFIX}/api/v1.0/documents/create-for-owner/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/children/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/tree/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/duplicate/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/ai-proxy/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/cors-proxy/"),
            format!("{PREFIX}/api/v1.0/users/"),
            format!("{PREFIX}/api/v1.0/users/?q=someone"),
            // A user id that is not a canonical uuid, and the collection
            // itself, both stay unreachable.
            format!("{PREFIX}/api/v1.0/users/not-a-uuid/"),
            format!("{PREFIX}/external_api/v1.0/documents/"),
            format!("{PREFIX}/admin/"),
        ] {
            for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
                assert_eq!(classify(method, &path, None), None, "reachable: {method} {path}");
            }
        }
    }

    #[test]
    fn history_is_readable_but_not_destroyable() {
        // The editor keeps a History panel, so denying these would leave dead UI.
        // Reading is document-scoped and safe; upstream's DELETE on the same
        // route is not an authority this bridge grants.
        for path in [
            format!("{PREFIX}/api/v1.0/documents/{DOC}/versions/"),
            format!("{PREFIX}/api/v1.0/documents/{DOC}/versions/v1.0_abc-def~1/"),
        ] {
            assert_eq!(
                classify("GET", &path, None),
                Some(Target::Document {
                    resource: DOC.into(),
                    write: false,
                    body: Body::None
                }),
                "history unreadable: {path}"
            );
            assert_eq!(classify("DELETE", &path, None), None, "history destroyable: {path}");
            assert_eq!(classify("POST", &path, None), None);
        }
        // A version id outside upstream's own declared character set is refused,
        // so this proxy never admits a path upstream's router would not route.
        assert_eq!(
            classify("GET", &format!("{PREFIX}/api/v1.0/documents/{DOC}/versions/a b/"), None),
            None
        );
        assert_eq!(
            classify("GET", &format!("{PREFIX}/api/v1.0/documents/{DOC}/versions//"), None),
            None
        );
    }

    #[test]
    fn an_id_that_is_not_a_canonical_uuid_v4_is_refused() {
        // Anything looser would disagree with either Neev's canonical_id or
        // y-provider's own v4 requirement, and a disagreement between the two is
        // precisely where a confused-deputy bug would live.
        for id in [
            "not-a-uuid",
            "B3B1F0D2-8C4A-4F5E-9A1B-2C3D4E5F6A7B",         // upper case
            "b3b1f0d28c4a4f5e9a1b2c3d4e5f6a7b",             // unhyphenated
            "b3b1f0d2-8c4a-1f5e-9a1b-2c3d4e5f6a7b",         // v1, not v4
            "",
        ] {
            assert_eq!(get(&format!("{PREFIX}/docs/{id}/")), None, "accepted id: {id}");
            assert_eq!(
                get(&format!("{PREFIX}/api/v1.0/documents/{id}/")),
                None,
                "accepted id: {id}"
            );
        }
    }

    #[test]
    fn the_socket_is_only_reachable_with_a_room_and_carries_that_document() {
        assert_eq!(
            classify(
                "GET",
                &format!("{PREFIX}/collaboration/ws/"),
                Some(&format!("room={DOC}"))
            ),
            Some(Target::Collaboration {
                resource: DOC.into()
            })
        );
        // No room, a malformed room, or a trailing path are all refused. The
        // room is only a claim; what is forwarded upstream is rebuilt from the
        // authorized resource, so this check exists to fail fast, not to be the
        // boundary.
        assert_eq!(classify("GET", &format!("{PREFIX}/collaboration/ws/"), None), None);
        assert_eq!(
            classify("GET", &format!("{PREFIX}/collaboration/ws/"), Some("room=nope")),
            None
        );
        assert_eq!(
            classify("GET", &format!("{PREFIX}/collaboration/ws/x"), Some(&format!("room={DOC}"))),
            None
        );
        // Upstream's connection-reset API is not exposed.
        assert_eq!(
            classify("POST", &format!("{PREFIX}/collaboration/api/reset-connections/"), None),
            None
        );
    }

    #[test]
    fn an_attachment_is_authorized_as_the_document_that_owns_it() {
        assert_eq!(
            get(&format!("{PREFIX}/media/{DOC}/attachments/file.png")),
            Some(Target::Media {
                resource: DOC.into(),
                key: format!("{DOC}/attachments/file.png")
            })
        );
        // A key that names no document cannot be authorized as one, so it is not
        // reachable at all — there is no unauthenticated object path.
        assert_eq!(get(&format!("{PREFIX}/media/loose-file.png")), None);
        assert_eq!(get(&format!("{PREFIX}/media/")), None);
    }

    #[test]
    fn nothing_outside_the_prefix_is_ours_and_traversal_is_refused() {
        // A prefix must be a whole segment, or `/document-engineering/` would be
        // captured and then served from the engine's artifact.
        assert_eq!(get("/document-engineering/docs/"), None);
        assert_eq!(get("/_next/static/chunk.js"), None);
        assert_eq!(get("/media/anything"), None);
        assert_eq!(get("/api/v1.0/documents/"), None);
        for path in [
            format!("{PREFIX}/../api/neev/session"),
            format!("{PREFIX}/docs/../../etc/passwd"),
            format!("{PREFIX}//api/v1.0/config/"),
            format!("{PREFIX}/media/{DOC}/..%2fsecret"),
            format!("{PREFIX}/media/{DOC}/..\\secret"),
        ] {
            assert_eq!(get(&path), None, "traversal accepted: {path}");
        }
    }

    fn asset(path: &str) -> Option<Target> {
        Some(Target::Asset { path: path.into() })
    }

    #[test]
    fn a_percent_encoded_asset_name_resolves_to_the_file_on_disk() {
        // The export contains literal bracketed names — `docs/[id]/` — and the
        // browser requests them percent-encoded. Refusing `%` outright, as the
        // structured routes do, 404s the editor's own page chunk and the app
        // never starts.
        assert_eq!(
            get(&format!("{PREFIX}/_next/static/chunks/pages/docs/%5Bid%5D-b6e91aea.js")),
            asset("_next/static/chunks/pages/docs/[id]-b6e91aea.js")
        );
        // Decoded exactly once: a doubly-encoded traversal must not survive one
        // decode into a live one.
        assert_eq!(get(&format!("{PREFIX}/_next/%252e%252e/secret")), None);
        assert_eq!(get(&format!("{PREFIX}/_next/%2e%2e/secret")), None);
        assert_eq!(get(&format!("{PREFIX}/_next/%2fetc%2fpasswd")), None);
        // Structured routes stay strict — an encoded id is an ambiguity that
        // buys nothing and invites two layers to disagree.
        assert_eq!(get(&format!("{PREFIX}/docs/%2e%2e/")), None);
        assert_eq!(get(&format!("{PREFIX}/api/v1.0/documents/%2e%2e/")), None);
    }

    #[test]
    fn the_static_export_is_reachable_for_reads_only() {
        assert_eq!(get(&format!("{PREFIX}/_next/static/chunk.js")), asset("_next/static/chunk.js"));
        assert_eq!(get(&format!("{PREFIX}/favicon.ico")), asset("favicon.ico"));
        assert_eq!(get(PREFIX), asset(""));
        // An asset route must never become a way to send a body to Docs.
        assert_eq!(classify("POST", &format!("{PREFIX}/_next/static/chunk.js"), None), None);
        assert_eq!(classify("PUT", &format!("{PREFIX}/anything"), None), None);
    }

    #[test]
    fn the_signed_in_user_and_frontend_config_are_the_only_document_free_calls() {
        assert_eq!(get(&format!("{PREFIX}/api/v1.0/users/me/")), Some(Target::Account));
        assert_eq!(get(&format!("{PREFIX}/api/v1.0/config/")), Some(Target::Account));
        // Not a licence for the rest of the user API. `onboarding-done` in
        // particular stays out **deliberately**, and this is the note for
        // whoever meets its 404 later: upstream mounts its own onboarding modal
        // over the editor when `is_first_connection` is true, and dismissing it
        // posts here. Admitting the route would be admitting a write route for a
        // flow Neev owns. The bridge sets `is_first_connection=False` when it
        // creates an account instead, so the modal never opens and there is
        // nothing to dismiss.
        assert_eq!(
            classify("POST", &format!("{PREFIX}/api/v1.0/users/onboarding-done/"), None),
            None
        );
        assert_eq!(get(&format!("{PREFIX}/api/v1.0/users/me")), None); // no trailing slash
        // The editor's boot write is a self-scoped language change at the
        // caller's own id. It throws when refused, so denying it broke the boot
        // rather than merely logging.
        assert_eq!(
            classify("PATCH", &format!("{PREFIX}/api/v1.0/users/{DOC}/"), None),
            Some(Target::AccountWrite { body: Body::LanguageOnly })
        );
        assert_eq!(Body::LanguageOnly.permitted(), ["language"]);
        // Not a licence for anything else on that object.
        assert_eq!(classify("DELETE", &format!("{PREFIX}/api/v1.0/users/{DOC}/"), None), None);
        assert_eq!(classify("GET", &format!("{PREFIX}/api/v1.0/users/{DOC}/"), None), None);
    }

    #[test]
    fn one_documents_route_cannot_reach_another_documents_data() {
        let Some(Target::Document { resource, .. }) =
            get(&format!("{PREFIX}/api/v1.0/documents/{DOC}/"))
        else {
            panic!("expected a document target");
        };
        assert_eq!(resource, DOC);
        assert_ne!(resource, OTHER);
    }
}
