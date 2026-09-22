# Neev document engine — La Suite Docs behind the Neev proxy

Decision and implementation plan for making La Suite Docs the engine behind **new** Neev
documents. Existing native documents are not migrated and keep their current engine; this is a
standing constraint, not a phase. Docmost is untouched.

This builds on the [evaluation](collaborative-documents-evaluation-2026-09-22.md) and the
[isolated preview](../lasuite-docs-preview/README.md), which established viability. What follows
is about integration, and every claim below is grounded in pinned upstream v5.7.0 source or in
this repository — the places where it is not are marked as open.

## The shape

One public entry, `https://app.neev.so`, terminated by the existing Caddy vhost. Neev's own
server is the **sole** route to Docs: the Docs backend, y-provider and frontend publish no
ports and sit on a private network. Everything under `/document-engine/` is reverse-proxied by
Neev after Neev has authorized the request against its own tables. Nothing else can reach Docs.

```
browser ──► Caddy (app.neev.so) ──► Neev server ──┬─► /document-engine/docs/…   frontend (static)
                                   authorizes     ├─► /document-engine/api/v1.0/ backend (Django)
                                   every request  ├─► /document-engine/collaboration/ws/  y-provider
                                                  └─► /document-engine/media/   backend-signed S3
```

A Neev `document` resource gains a mapping row to a Docs document UUID. The Projects UI embeds
`/document-engine/docs/<uuid>/` in a same-origin iframe at full available height. The upstream
editor runs unmodified, so the durable save path — `GET/PATCH /api/v1.0/documents/<id>/content/`,
written by the editor — is preserved exactly as upstream intends.

## Decision 1 — identity: Django RemoteUser behind a trusted proxy

**Chosen:** upstream's supported settings seam — `django.contrib.auth.middleware.`
`PersistentRemoteUserMiddleware` plus a small `RemoteUserBackend` subclass, both configured by a
settings module mounted into the stock backend image. The preview already proves this seam works:
`preview_settings.py` is bind-mounted and selected by `DJANGO_SETTINGS_MODULE` with no image
change. The Neev proxy authenticates the Neev session it already has, then sets the identity
header on the forwarded request; Django mints its own `docs_sessionid`.

This replaces the OIDC provider that earlier research pointed at. The reasoning that changed:
we need per-request Neev-source-of-truth authorization on every proxied call *anyway* — OIDC
would have solved login and left authorization, ACL drift and revocation exactly where they were.
Given that, a full OIDC OP in Neev is a large new security surface bought for nothing. Header
auth behind a trusted proxy is only "cosmetic" when the origin is reachable without the proxy;
here it is not, which is the condition that makes `RemoteUserMiddleware` a legitimate Django
mechanism rather than a bypass.

The conditions that make it sound, all of which are requirements on this work:

- Docs publishes no port; the private network is its only ingress.
- The Neev proxy **strips every inbound identity header** from the client request before adding
  its own. A forged `X-Neev-*` from the browser must be impossible to distinguish from absent,
  because it is deleted before any handler sees it.
- Neev authorizes first, forwards second. A request that Neev denies is never forwarded.
- A Docs session cookie is not by itself an entitlement. Every proxied request is authorized
  against the caller's **Neev** session, so a still-valid `docs_sessionid` presented without a
  live Neev session is refused at the proxy and never reaches Docs. The Docs cookie is how Docs
  recognises a request the proxy already approved — it is not a second way in.

**Why `Persistent`RemoteUserMiddleware and not the plain one.** This is forced by upstream source,
not preference. `y-provider/src/api/collaborationBackend.ts` forwards exactly three headers to the
backend when it authorizes a socket:

```ts
headers: {
  cookie: requestHeaders['cookie'],
  origin: requestHeaders['origin'],
  'X-Y-Provider-Key': Y_PROVIDER_API_KEY,
}
```

Arbitrary headers are dropped. So the identity header cannot reach the backend on the
collaboration path, and that path must authenticate by the `docs_sessionid` cookie — which is
also what the preview observed (`4001 No cookies`). Plain `RemoteUserMiddleware` logs a session
out whenever the header is absent, which would break every y-provider callback. The persistent
variant is built for precisely this "header at login only" deployment.

`core.models.User` carries `sub` (unique), `email`, `full_name`, `short_name`, `language`, with
`USERNAME_FIELD = "admin_email"`. The backend subclass therefore resolves on `sub` rather than the
default username field, with `sub` set to the Neev user's stable identifier, namespaced by
organization. The default `RemoteUserBackend` lookup is not usable unchanged.

## Decision 2 — authorization: Neev per-request, no ACL dual-write

Neev's `content/access.rs::authorized(tx, actor, id, write)` is the single decision function today
and stays the single decision function. The proxy calls it on every request and every socket
event; it is never cached across requests, so there is no interval during which Neev's answer and
the enforced answer disagree.

- **HTTP.** Deny → 403, not forwarded. Read-only → mutating methods on the document are refused at
  the proxy, so the read/write distinction is enforced without consulting Docs' ACL at all.
- **Websocket.** y-provider derives `connectionConfig.readOnly` from `abilities.update` returned by
  the backend, so the socket's write capability is decided inside Docs. Neev's role is therefore
  projected just-in-time into the Docs access row for that user, inside the same authorization
  step, immediately before the upgrade is forwarded, and only written when it differs. It is
  re-derived on every connect — a projection, never a second source of truth.
- **The socket is bound to one document, and every frame is checked.** Authorizing the handshake
  is not sufficient: the Hocuspocus wire format names a document in each message, so one socket
  can address documents the handshake never covered. The proxy binds the connection to the single
  document id it authorized, decodes the leading document-name field of every frame in **both**
  directions, and closes the connection on any other name. This decodes one leading field of an
  existing format to enforce a boundary; it does not reimplement the protocol, and the official
  client and server stay on both ends.
- **Revocation.** The proxy holds the socket and re-runs the same recheck on a bounded interval and
  on inbound and outbound data, following the pattern `neev/http/realtime.rs` already uses for chat
  SSE (session + org + resource recheck before every delivery, plus heartbeat). Loss of access,
  session expiry, organization switch or idle expiry closes the socket. A downgrade to read-only
  also closes it; the client reconnects and is re-projected as a reader. No authorization result is
  cached across a recheck, and any failure to evaluate closes the socket — fail-closed, never
  fail-open.
- **Routes are an allowlist, not a denylist.** Only the endpoints the embedded editor actually
  needs are proxied; everything else under `/document-engine/` is refused by default. A denylist
  would have to be re-audited against every upstream release and would leak whatever it forgot.
  This closes the generic surfaces by construction: document list and search, user search, and
  sub-document creation are not reachable, so no other organization's or another user's private
  document can be enumerated through them. Sharing actions may not widen a Neev ACL, so `accesses`,
  `invitations` and `link-configuration` are outside the allowlist. Hiding them in the UI is
  cosmetic and is not the control; the proxy is.
- **Attachments are gated like everything else.** Media is fetched through the proxy and authorized
  against the mapped resource on each request. The object store is not publicly reachable and is
  not fronted by an unauthenticated nginx location. Long-lived presigned URLs are not used: a URL
  that outlives the grant that produced it is a revocation hole, so the signature lifetime stays
  short and the proxy re-authorizes rather than handing out a durable capability.
- **Cross-organization and forged ids.** The mapping is looked up by `(org_id, resource_id)` and
  the Docs UUID is derived from it. A Docs UUID supplied by the client is never trusted, so a
  forged or cross-org id resolves to nothing and is denied before forwarding.
- **Legacy routes cannot be a bypass.** The existing native content routes refuse any resource
  that carries a Docs mapping, so a La Suite-backed document cannot be read or written through
  the old path. Documents without a mapping keep working exactly as now.

## Decision 3 — one origin, `/document-engine/` (settled with root)

`app.neev.so` resolves to the host and its certificate is in place; `docs.neev.so` has no record
and there are no DNS credentials, so a second subdomain is not available. A second origin would
also make the iframe's cookies third-party. The iframe is therefore same-origin, and the only
question was how to keep two Next.js static exports from colliding.

Upstream v5.7.0 has **no `basePath`**, is `output: 'export'` with `trailingSlash: true`, and ships
a workbox service worker unless `NEXT_PUBLIC_SW_DEACTIVATED=true`. Served at the origin root that
service worker would take scope `/` and intercept every Neev route — a genuine hazard, not a
tidiness concern. Serving Docs at the root and moving *Neev's* assets aside was considered and
rejected: it leaves Docs owning the origin root and trades a clean namespace for a permanent
collision risk. A non-standard port isolates cleanly but is firewall-hostile.

Settled: a custom frontend build with `basePath: '/document-engine'`, service worker disabled,
MIT build. Upstream's own `Dockerfile` already exposes `API_ORIGIN`, `SW_DEACTIVATED` and
`PUBLISH_AS_MIT` as build args, so this is an extension in the shape upstream already supports,
not a fork of anything load-bearing. The build runs on a developer machine and ships the static
artifact; the server's 3.5 GiB free is not spent on a frontend build.

Frontend packaging is owned by a separate worker on `neev-docs-ui-20260922`
(`deploy/lasuite/frontend/`). The contract between us:

| | path |
|---|---|
| editor | `/document-engine/docs/<uuid>/` |
| API | `/document-engine/api/v1.0/` |
| collaboration socket | `/document-engine/collaboration/ws/` |
| media | `/document-engine/media/` |

`baseApiUrl()` is `${NEXT_PUBLIC_API_ORIGIN || window.location.origin}/api/v1.0/`, so the API path
above needs `NEXT_PUBLIC_API_ORIGIN` to carry the prefix; the socket and media URLs come from the
backend's `COLLABORATION_WS_URL` and media settings, which are mine to configure.

Neev's page CSP (`http/static_files.rs::app_csp`) is `default-src 'none'` with `frame-ancestors
'none'` and no `frame-src`. Embedding needs a narrow `frame-src 'self'` on the document route and a
matching `frame-ancestors 'self'` on the Docs responses — scoped to that route, not relaxed
globally.

## Creation

Creating a document in a project or subfolder stays a native Neev action: it creates the
`neev_resources` row as it does now, and additionally creates the Docs document and the mapping.
No title prompt; the title is editable afterwards and is Neev's, projected to Docs.

Idempotency reuses what exists rather than inventing it: `resources::receipt` /
`record_receipt` over `neev_requests(user_id, action, request_id, body_hash, resource_id)` already
returns the original resource for a repeated `request_id` and rejects a repeated id with a
different body as a conflict. The Docs document and mapping are created in the same transaction
boundary, so a retry cannot leave a second Docs document reachable, and a failure surfaces as a
clear, retryable error rather than a half-made resource.

One consequence of upstream's design is worth stating because it looks like a bug otherwise: a
document created through the API has **no** Yjs state until the editor's save path runs. The
preview established this. An empty new document is therefore expected, not evidence of a broken
create.

## Rollback

The feature flag governs **creation only**. Turning it off stops new documents being created on
the engine; it does not withdraw the ones already created. An engine-backed document stays
readable and editable whenever the engine is reachable, and never silently falls back to the
native editor — a mapped resource has no native content, so a fallback would render a blank
document and look like data loss.

Rolling the Neev image back must therefore leave engine-backed documents working, which means the
mapping schema has to be readable by the previous image rather than merely present. The schema
change is additive and the prior image must tolerate it; that is a property to rehearse, not to
assert, and the rehearsal is part of acceptance below.

## What is not settled

- Attachment upload was never exercised in the preview — its `/media/` proxy pointed at HTTPS
  while MinIO spoke HTTP. It has to be proven on this deployment, not assumed from it.
- Whether an **already-open** socket is dropped at the moment access is revoked was not tested
  upstream, and `/collaboration/api/reset-connections/` was never exercised. This design does not
  depend on upstream doing it: the proxy closes the socket itself. That is the thing to verify.
- Search, agent document context, and export/backup for La Suite-backed documents are not part of
  this change and are not claimed to work yet.

## Acceptance

Two genuinely separate Neev accounts, not one session in two tabs. Both edit the same document
concurrently; extend and resize a table; rename; reload; confirm convergence and that the stored
server state — not the browser's copy — carries the edits. A viewer cannot write over HTTP or over
the socket. A member of another organization is refused. Revoking access closes the open socket
and fails the next HTTP call. Logout revokes the session. Legacy native documents are unchanged and
the legacy routes refuse mapped resources. Then the same, over real HTTPS, after a backup and a
rollback rehearsal, with root's security review before any production change.
