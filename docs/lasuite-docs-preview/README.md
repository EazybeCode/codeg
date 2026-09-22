# La Suite Docs — isolated preview and evaluation

An isolated, disposable preview of [La Suite Docs](https://github.com/suitenumerique/docs)
v5.7.0, run to answer one question: **is this a sound basis for documents in
Neev?** It holds no production data, is reachable only over a loopback tunnel,
and shares nothing with the live Neev or Docmost stacks.

This directory is everything needed to reproduce it, plus what the evaluation
actually established — including what it did not.

## Verdict

Yes, on the evidence below. Authentication, the permission boundary, real-time
collaboration between two separate accounts, reconnection and server-side
persistence all behave correctly against a stock OIDC provider, with no product
changes. The work left is integration, not viability.

## What was verified

All of it against two **separate accounts**, not one session in two tabs — a
single session cannot demonstrate a permission boundary.

**Authentication**
- both accounts complete the real authorization-code flow and reach
  `/api/v1.0/users/me/` → **200**, with distinct user ids
- unauthenticated `/users/me/` → **401**

**Permissions, over the REST API**
- owner creates a document → **201**, reads it → **200**
- the second account is refused on that document → **403**
- link settings applied via `PUT /documents/<id>/link-configuration/`, read back
  as stored, and confirmed by an unauthenticated **200**
- withdrawing access → the second account drops to **403**

**Permissions, over the collaboration socket** — this one needs care. The HTTP
upgrade returns `101` for the owner, for a user the API refuses, **and for a
request with no cookie at all**. `101` is therefore not evidence of access, and
reading it as such would be wrong in both directions. Authorization happens in
the message exchange after the upgrade, where, using the official client:

| identity | outcome |
|---|---|
| owner | authenticated, document state delivered |
| second signed-in user (403 at REST) | **permission denied** |
| forged session cookie | **permission denied** |
| no cookie | closed, `4001 No cookies` |
| after access was withdrawn, **connecting anew** | **permission denied** |

On that last row: what was tested is a **new** connection made after access was
withdrawn. Whether an **already-open** socket is dropped at the moment access is
revoked was **not** tested, and `/collaboration/api/reset-connections/` was
never exercised. Do not read the table as evidence that live sessions are cut
off — that is an open question.

**Two accounts editing at once** (official `HocuspocusProvider`, pinned to the
preview's own server build 3.4.4 and yjs 13.6.32):
- both write concurrently, neither waiting → **converged to identical state**,
  both edits present on both sides
- one disconnects, edits while away, reconnects → **the offline edit merged**,
  and both converged again
- a **fresh client that had never seen the document** connects and receives all
  three edits

These two results are **separate proofs about different things**, and neither
substitutes for the other. The convergence run above is headless: it proves
live relay, merge and reconnect between two accounts, and proves **nothing**
about durability, because headless clients never run the editor's save path.
The persistence result below is a browser-authored document, and proves
durability only. Reading either as the other was the single most misleading
mistake available here.

**Server-side persistence.** A browser reload proves little, since the editor
keeps a local copy. So the stored state was fetched from the server and decoded
independently. For the document edited in a browser:

- 2254 bytes of Yjs state stored, shared type `document-store`
- text recovered: *"La Suite Docs preview — isolated test document. Table resize test"*
- structure: 1 table, 3 rows, 9 cells
- column widths, per row, in order: `[215, none, none]`, `[215, none, none]`,
  `[215, none, none]` — the resized first column carries an explicit width in
  every row and the untouched columns carry none. The resize persisted exactly.

## Findings that shape any integration

**1. The collaboration service is a relay, not the store.** It keeps documents
in memory and is empty after a restart — by design, not misconfiguration. The
durable copy lives behind `PATCH/GET /api/v1.0/documents/<id>/content/`, written
by the **editor**. Consequences worth knowing up front:

- headless clients never persist anything, because they do not run the editor's
  save path. A backend-authored document will not appear until something runs
  that path.
- `content` is **not** a field on the document serializer. Looking for it there
  and concluding nothing is stored would be wrong.

**2. OIDC is split-horizon and unforgiving.** Front-channel endpoints
(authorization, logout) are where the *browser* is sent and must use the
browser-reachable address. Back-channel endpoints (token, userinfo, **jwks**)
are called by the *backend*, where `localhost` is the backend container itself.
Getting JWKS wrong produces a `500` on the callback, not a login error.

**3. The identity provider's issuer must be pinned.** With a dynamic hostname it
derives the issuer from whichever host the request arrived on, so a token minted
via the browser is rejected at userinfo when the backend calls it over the
internal network — the issuers disagree, and it surfaces as an opaque `401`.

**4. The collaboration socket authenticates by session cookie**, and refuses a
connection carrying none with `4001 No cookies`. Any Neev-side proxying must
preserve that cookie.

**5. `Production` hardcodes HTTPS.** `SECURE_SSL_REDIRECT`,
`SESSION_COOKIE_SECURE` and `CSRF_COOKIE_SECURE` are set in the class and ignore
environment overrides. `Development` is not a substitute — it needs
`django_extensions`, which the production image does not ship, so the container
will not start. `preview_settings.py` subclasses Production and overrides those
three flags only.

**6. Keycloak issues `AUTH_SESSION_ID` and `KC_RESTART` as `Secure;
SameSite=None`** regardless of the realm's `sslRequired`. Browsers accept them
on `localhost` because that is a secure context; non-browser clients drop them
and then cannot complete a login. This is a property of scripted clients, not a
product defect.

## Limitations of this preview

- `preview_settings.py` relaxes three TLS flags so a plain-HTTP loopback tunnel
  works, so production cookie/TLS behaviour is **not** exercised here.
- `/media/` is proxied to `https://${S3_HOST}` while MinIO speaks plain HTTP, so
  **attachment upload is untested**. Core editing is unaffected.
- Keycloak runs `start-dev` on H2 — appropriate for a disposable preview and for
  nothing else.
- Concurrency was tested with two writers, not at scale.

## Reproducing it

```bash
./generate-env.sh /opt/neev-lasuite-preview     # creates every secret on the host
cd /opt/neev-lasuite-preview
docker compose up -d --wait --wait-timeout 120 postgresql redis minio keycloak
docker compose run --rm backend python manage.py migrate   # not run by the image
docker compose up -d --wait --wait-timeout 180
```

The order matters. Migrations are not run by the image, and the backend's
healthcheck runs `manage.py check` against a database that has no schema yet,
so bringing the whole stack up with `--wait` in one step can sit there until it
times out on an unhealthy backend. Dependencies first, migrate in a throwaway
container, then the rest.

Then tunnel and open it:

```bash
ssh -L 8100:127.0.0.1:8100 -L 8180:127.0.0.1:8180 <host>
# http://localhost:8100
```

Use **`localhost`**, not `127.0.0.1` — that is the host the callback and the
app's configured origin both use, and a mismatch is rejected as an invalid
`redirect_uri`.

Needs roughly 4 GB of disk for images and about 2 GB of RAM across the stack.

Teardown removes only this stack:

```bash
cd /opt/neev-lasuite-preview && docker compose down -v
```

## Not committed

No secret is in this directory. `generate-env.sh` creates the client secret,
both account passwords, database, object-store, admin and collaboration secrets
on the host, and renders the realm from `keycloak/realm.template.json` at that
point. The two preview accounts use the reserved `.invalid` domain and are not
real identities.

Modes, stated exactly rather than rounded off to "0600":

- `env.d/` — directory `700`, files `600`. Read by the docker daemon as root,
  never by a container.
- `keycloak/realm.json` — file **`644`**, inside a **`700`** directory. It holds
  secrets but is bind-mounted into Keycloak and must be readable by that
  container's user; the restrictive parent keeps it unreachable from the host,
  and a bind mount is resolved by the daemon as root regardless.
- `compose.yaml`, `preview_settings.py`, `default.conf.template` — `644`, and
  contain nothing secret. The frontend runs as uid 101 and the backend as uid
  1000; leaving these at a `umask 077` default makes nginx exit with
  "Permission denied".

### About the realm template

`keycloak/realm.template.json` deliberately carries **no comment fields**. It is
rendered straight into the file Keycloak imports, and a realm import is not a
place to find out whether an unknown property is tolerated. Everything worth
saying about it is here instead:

- Every `__PLACEHOLDER__` is filled by `generate-env.sh` from material generated
  on the host.
- `sslRequired` is `none` because the preview is reached over a plain-HTTP
  loopback tunnel. That does **not** stop Keycloak issuing `AUTH_SESSION_ID` and
  `KC_RESTART` as `Secure; SameSite=None`.
- Both `localhost` and `127.0.0.1` are registered, because the app builds its
  callback from one spelling and a mismatch is refused as an invalid
  `redirect_uri`.

The rendered realm was checked by importing it into a throwaway Keycloak on an
isolated network with no published ports: *"Realm 'docs' imported"*, *"Import
finished successfully"*, server listening. The container was removed afterwards.

## Still open

Integrating this with Neev's own identity, and any migration of existing
documents, is deliberately out of scope here and not designed or attempted.
Nothing in this evaluation should be read as a migration plan.
