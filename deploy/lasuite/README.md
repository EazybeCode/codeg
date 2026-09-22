# Neev document engine — La Suite Docs backend bridge

The Django half of running [La Suite Docs](https://github.com/suitenumerique/docs) v5.7.0 as
Neev's document engine: a settings package mounted into the **stock** upstream image, and an
isolated compose stack with no public entry point of its own.

It exists because Docs authenticates with OIDC and Neev already knows who the user is. Rather than
give Docs a second identity provider, the Neev proxy authenticates every request against Neev's own
ACL and then asserts the result in a header. This package is what makes Docs accept that, safely.

Owned here: `backend/`, `compose.yaml`, `generate-env.sh`, this file. Not here: the Rust proxy and
route allowlist (`src-tauri/`), the frontend artifact (`deploy/lasuite/frontend/`), the Caddy route.

---

## The trust boundary

```
browser ──► Neev (sole public entry; authorises EVERY request against the native ACL)
              │  strips client identity headers, injects its own
              ├─ HTTP  ──► backend  :8000   /document-engine/api/v1.0/, /media/
              ├─ WS    ──► y-provider :4444 /collaboration/ws/
              └─ bridge──► backend  :8000   /_neev/v1/...  (credentialled, private)
```

Three properties hold this up, and losing any one of them voids the others:

1. **No service publishes a port.** Neev is the only way in. A published port would let anything on
   the host set `X-Neev-Remote-User` and be anyone. This is why `compose.yaml` has no `ports:`
   anywhere, and why that must not be "temporarily" added.
2. **The proxy strips client identity headers** — `x-neev-remote-user`, `-email`, `-name`,
   `x-neev-bridge-key`, `remote-user`, `x-remote-user`, `x-forwarded-*`, `x-real-ip`,
   `x-y-provider-key` — before reading or forwarding anything, then sets its own.
   `X-Forwarded-Proto` is in that list because upstream's `Production` sets
   `SECURE_PROXY_SSL_HEADER`, so a client value would talk Django out of its own TLS assumption.
3. **Docs' ACL is a projection, never the decision.** Neev authorises; the bridge writes the
   matching `DocumentAccess` so upstream code — including y-provider, which reads
   `abilities.update` to decide whether a socket may write — agrees with a decision already made.

## Identity

| header | value | role |
|---|---|---|
| `X-Neev-Remote-User` | `neev:<org_id>:<user_id>` | the identity. Becomes `core.models.User.sub` |
| `X-Neev-Remote-Email` | verified email | advisory; written, never used to resolve a user |
| `X-Neev-Remote-Name` | display name | advisory |

Segments are `[0-9A-Za-z_-]{1,64}` and cannot contain `:`, so `neev:1:23` and `neev:12:3` are
different principals and cannot be made to collide. `neev:service:*` is reserved for the
server-to-server principal and is **refused on the header path**, so a browser can never assert the
account that owns every document.

Two things in here are not preference, they are forced by upstream's code:

- **The middleware compares on `sub`, not `get_username()`.** `core.models.User` sets
  `USERNAME_FIELD = "admin_email"`, which is NULL for every Neev user, so Django's stock
  `PersistentRemoteUserMiddleware` would find a mismatch on *every* request, log out, log back in,
  and hand the browser a fresh `docs_sessionid` and CSRF token each time. Parallel requests would
  race and the collaboration socket would authenticate with a session that no longer exists.
  `tests/test_middleware.py` asserts stability both ways: ours is stable, and the stock one rotates.
- **The middleware is persistent (no header is not a logout).** `y-provider` forwards only `cookie`,
  `origin` and `X-Y-Provider-Key` to the backend, so the identity header is absent by design on
  every collaboration callback.

## Provisioning and ACL projection — `/_neev/v1/`

Server-to-server only: on the private network, credentialled with `X-Neev-Bridge-Key`
(constant-time compare, refused below 32 characters, `503` if unset), never in the proxy's route
allowlist, and session-less — these paths neither read nor set a cookie, so CSRF does not apply to
them. Upstream's CSRF protection on every browser-reachable route is untouched.

| endpoint | body | returns |
|---|---|---|
| `POST /_neev/v1/documents/provision` | `{id, title, sub, email?, name?}` | `{id, created}` |
| `POST /_neev/v1/documents/<id>/access` | `{sub, role: reader\|editor, email?, name?}` | `{role, changed}` |
| `DELETE /_neev/v1/documents/<id>/access` | `{sub}` | `{revoked}` |
| `POST /_neev/v1/documents/<id>/title` | `{sub, title}` | `{title, changed}` |
| `POST /_neev/v1/sessions/mint` | `{sub, ttl_seconds?}` | `{cookie_name, session_key, expires_in}` |
| `POST /_neev/v1/sessions/resolve` | `{session_key}` | `{sub}` or `{sub: null}` |
| `GET /_neev/v1/health` | — | `{status}` |

Refusals are `{"error": "<code>"}`: `400` invalid_json / invalid_subject / invalid_document_id /
invalid_title / invalid_role, `401` unauthorized, `404` unknown_document, `409`
not_an_engine_document / organization_mismatch / privileged_access, `413` body_too_large,
`503` bridge_disabled.

**The document id is the Neev resource id.** Both are UUIDv4; there is no mapping table and nothing
to forge. Provisioning is idempotent and converges under a race, so a retry after a partial failure
repairs rather than duplicates.

**Nobody is impersonated, and no human is privileged.** Every engine document is owned by one
ordinary service principal, `neev:service:engine`. Granting access is an ordinary owner action by
the principal actually making the call — not a request forged in the creator's name — and projected
roles are only `reader` and `editor`. So even a Docs control that escaped the proxy's allowlist
could not reconfigure sharing or delete a document: sharing stays a Neev concept.

**The title endpoint is reconciliation, not a rename.** Neev's resource row is authoritative — it
is what Projects lists and what the ACL is evaluated against — so the two copies can only diverge
when one of the pair of writes fails after the other succeeded: a rename Docs rejected, or a native
update that lost a revision conflict. The repair always runs in one direction, Neev's value onto the
Docs copy, which is why the endpoint takes a title rather than negotiating one. It bounds an
over-long title to Docs' 255 rather than refusing it, so a legitimate Neev title can never make the
repair itself fail.

**The websocket gets a session the client never held.** On HTTP the proxy asserts identity per
request and the middleware repairs a mismatched session. On the socket there is no such step — Docs
authorises from `docs_sessionid` alone — so a client holding a native session as A and a Docs cookie
belonging to B would open a socket as B. `sessions/mint` returns a session the proxy derives itself
and forwards *instead of* the browser's cookie; `sessions/resolve` answers "whose cookie is this"
for verification and tests. The proxy uses one of the two, and caches a minted key per subject until
roughly half of `expires_in` so sessions do not proliferate.

## Media — how attachments actually work at v5.7.0

Measured against the running canary, not read off the docs. Every status below was
observed.

```
POST /api/v1.0/documents/<id>/attachment-upload/   multipart, session cookie + X-CSRFToken + Origin
  -> 201 {"file": "/api/v1.0/documents/<id>/media-check/?key=<URL-ENCODED key>"}
  -> 403 without a CSRF token                       (upstream's CSRF is live on this route)

GET  /api/v1.0/documents/<id>/media-check/?key=…    -> 200 {"status":"ready","file":"/media/<key>"}

GET  /media/<key>                                   -> 404 FROM DJANGO. Django never serves media
                                                       in production; upstream's nginx does.

GET  /api/v1.0/documents/media-auth/                 header X-Original-URL: /media/<key>
  -> 200 "authorized" + Authorization, X-Amz-Date, X-Amz-Content-SHA256   (SigV4, no URL, no expiry)
  -> 403 for a user without access, anonymous, a missing X-Original-URL, or a traversal path

GET  http://minio:9000/<bucket>/<key>  with those three headers
  -> 200, the bytes, Content-Type from libmagic      anonymous: 403
```

So the bytes are fetched **server-side** with headers Django computes per request. There is no
presigned URL and no redirect anywhere in the flow, and that is the point: authorization is
re-evaluated on every single fetch, and nothing shareable is ever minted. Any implementation that
redirects a browser to the object store — presigned or not — replaces realtime authorization with a
bearer URL and must not be built.

Three measured facts that a media implementation has to handle, none of them obvious:

- **`media-auth` needs only the identity header.** No Docs session cookie is required (verified:
  200 with the header alone), which keeps the subrequest cheap and stateless.
- **`Content-Disposition` is not on the object.** Upload stores it, and then the malware-detection
  callback (`core/malware_detection.py`, default `DummyBackend`) rewrites the metadata with
  `copy_object(..., MetadataDirective="REPLACE")` without carrying it over — proven directly:
  present after upload, `None` after the copy. Every attachment therefore loses its disposition at
  the exact moment it becomes readable, including the ones upstream marked unsafe. Whatever serves
  `/media/` must set the disposition itself.
- **SVG is not treated as unsafe.** `image/svg+xml` is absent from `DOCUMENT_UNSAFE_MIME_TYPES`, so
  an SVG carrying `<script>` uploads with `is_unsafe` false and `Content-Type: image/svg+xml`.
  Served same-origin and navigated to directly, it executes in the Neev origin. `text/html` *is*
  flagged (key suffixed `-unsafe`), and a disguised upload is caught by libmagic rather than by the
  declared type — a `.png` containing HTML is stored as `text/html` and marked unsafe.

The object GET also returns `x-amz-meta-owner`, `x-amz-meta-status` and `x-amz-meta-is_unsafe`.
`owner` is a Docs user id; a proxy that copies response headers blindly hands it to the browser.

## Install

```bash
./generate-env.sh                     # production → /opt/neev-document-engine
./generate-env.sh --canary            # loopback canary → /opt/neev-document-engine-canary
```

Secrets are generated on the host, never printed and never committed; `env.d/` is `700` with `600`
files inside a `700` parent. The script refuses to overwrite an existing `env.d`, because re-keying
invalidates every live session and bridge credential at once.

```bash
cd /opt/neev-document-engine
docker compose up -d --wait --wait-timeout 120 postgresql redis minio
docker compose run --rm backend python manage.py migrate
docker compose up -d --wait --wait-timeout 180
```

Migrations first: the backend's healthcheck is `manage.py check`, which needs a schema, so bringing
everything up at once can time out as unhealthy.

### Two stacks on one host

Every compose name the stack owns derives from `STACK_NAME` in its `.env`: the project, the private
network (`${STACK_NAME}-internal`) and the volume prefix. The generator picks
`neev-document-engine-prod` or `neev-document-engine-canary`, and prints what compose resolved:

```
resolved compose names — must differ from every other stack on this host:
  project    neev-document-engine-canary
  network    engine   neev-document-engine-canary-internal [private]
  network    neev     neev-docs-native-test                [external — shared on purpose]
  volume     postgres neev-document-engine-canary_postgres
```

This is not tidiness. Two stacks sharing a compose project means `up` in one directory recreates
the other's containers; two sharing a private network means both backends answer to `backend:8000`
and which one a request reaches is a race. Only the external Neev network is shared, deliberately,
and it is the stack's one trust edge. A stack that is already running keeps the name it was created
with — changing `STACK_NAME` on a live stack orphans its containers, network and volumes.

### Env format — one trap worth knowing

`DJANGO_ALLOWED_HOSTS` and `DJANGO_CSRF_TRUSTED_ORIGINS` are **comma-separated, not JSON**.
django-configurations' `ListValue` splits on `,`, so `["a","b"]` is stored verbatim as
`['["a"', '"b"]']`. It looks configured and is not: no `Origin` header can ever equal a trusted
origin in that form, so every unsafe browser request 403s. This was found by running against the
real image, and `tests-container/tests_bridge.py` asserts the parsed values so it cannot come back.

### Configuration invariants, enforced at startup

`NeevProduction.post_setup` refuses to start — loudly, with the reason on stderr — if the
middleware is mis-ordered, `CsrfViewMiddleware` is gone, the OIDC backend is still enabled,
`SESSION_COOKIE_NAME` is not `docs_sessionid`, cookies are not `Secure`, or `MEDIA_BASE_URL` /
`COLLABORATION_WS_URL` have been set. Those last two are deliberately unset: the frontend's
`backendUrl()` already carries `/document-engine`, so setting them would double-prefix media and
pin the socket to one host.

`NeevCanary` is the only configuration with `Secure` cookies off, for a plain-HTTP loopback canary.
It refuses to start without `NEEV_CANARY=1`, so a copied env file cannot quietly select it in
production.

## Tests

```bash
backend/tests/run-local.sh                  # 59 tests, real Django 5.2.17, no container
cd /opt/neev-document-engine-canary && backend/tests-container/run.sh   # 56 tests, real image
```

The local suite runs against a User model that mirrors `core.models.User` where the bridge depends
on it — `USERNAME_FIELD = "admin_email"`, a unique `sub`, and `save()` calling `full_clean()`. The
in-image suite covers what a mirror cannot: the real settings module, real provisioning, upstream's
own `get_abilities`, and the real Redis session store. Django's test runner creates and destroys its
own `test_docs` database, so a stack's data is never touched.

## Backup and restore

Both round trips below have been run end to end on the canary: a provisioned document, its two
access rows and an object came back intact.

```bash
cd /opt/neev-document-engine
# database
docker compose exec -T postgresql pg_dump -U docs -Fc docs > docs-db.dump
# object store
docker compose run --rm -v "$PWD/docs-media:/backup" --entrypoint sh createbuckets -c \
  'mc alias set engine http://minio:9000 $MINIO_ROOT_USER $MINIO_ROOT_PASSWORD &&
   mc mirror --overwrite engine/$BUCKET_NAME /backup'
```

Restore — into a throwaway database first, which is also how the restore itself gets verified:

```bash
docker compose exec -T postgresql psql -U docs -d postgres \
  -c "CREATE DATABASE docs_restore_check OWNER docs;"
docker compose exec -T postgresql sh -c 'cat > /tmp/r.dump' < docs-db.dump
docker compose exec -T postgresql pg_restore -U docs -d docs_restore_check /tmp/r.dump
docker compose exec -T postgresql psql -U docs -d docs_restore_check -c \
  "SELECT count(*) FROM impress_document;"
# for a real restore, stop the backend, restore into `docs`, then start it again
docker compose run --rm -v "$PWD/docs-media:/backup" --entrypoint sh createbuckets -c \
  'mc alias set engine http://minio:9000 $MINIO_ROOT_USER $MINIO_ROOT_PASSWORD &&
   mc mirror --overwrite /backup engine/$BUCKET_NAME'
```

The database and the object store must be restored from the same moment: an attachment row without
its object is a broken image, and an object without its row is unreachable and unaccounted for.

## Not verified here

- **Nothing has run in a browser.** Every claim above comes from the test suites and from HTTP calls
  against the real image, which is not the same as a person opening a document.
- **Attachment upload was never exercised**, here or in the preview.
- The production stack has not been deployed; only the isolated canary has been run.
- Django-served responses keep upstream's CSP `frame-ancestors 'none'`. The framed document is the
  static artifact served by the proxy, so `SAMEORIGIN` and `frame-ancestors 'self'` belong **there**
  — Django does not provide them for the embed.
