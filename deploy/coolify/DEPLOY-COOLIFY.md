# Deploy Eazybe WorkOS on Coolify + Postgres

This guide is for a trusted internal pilot with GitHub login and Postgres. It is
not a public SaaS isolation guide: the bearer-token and agent-runner boundaries
described under Security still apply.

## 0. Provision

- A Coolify installation managed by your organization.
- A domain you control with DNS pointed at that installation (for example,
  `workos.example.com`).

## 1. Add the Postgres database (Coolify resource)

- Coolify → your project → **+ New** → **Database** → **PostgreSQL 16**.
- Create it. Coolify gives an **internal connection URL** like
  `postgres://<user>:<pass>@<service-name>:5432/<db>`. Copy it — that's your
  `DATABASE_URL`. (Internal-only; not exposed publicly. Good.)
- Enable Coolify's [scheduled backups](https://coolify.io/docs/databases/backups)
  on this database.

## 2. Add the app (from your GitHub repo)

- Coolify → **+ New** → **Application** → **Public/Private Repository**.
- Repo and branch: your organization's reviewed WorkOS fork and release branch.
- Use Coolify's [Dockerfile build
  pack](https://coolify.io/docs/applications/build-packs/dockerfile); the repo's
  Dockerfile builds the frontend and Rust server.
- Port: **3080**. Set your domain (`workos.example.com`) and enable TLS.

## 3. Environment variables (Coolify → the app → Environment)

```
DATABASE_URL      = <the Postgres internal URL from step 1>
CODEG_TOKEN       = <openssl rand -hex 24>      # break-glass/admin token
CODEG_HOST        = 0.0.0.0
CODEG_PORT        = 3080
CODEG_STATIC_DIR  = /app/web                    # (Dockerfile already sets this)
CODEG_DATA_DIR    = /data                       # persistent runtime data
GITHUB_CLIENT_ID     = <your-github-app-client-id>
GITHUB_CLIENT_SECRET = <your-github-app-client-secret>
GITHUB_CALLBACK_URL  = https://workos.example.com/auth/github/callback
WORKOS_APP_ORIGIN    = https://workos.example.com
WORKOS_ENC_KEY       = <openssl rand -hex 32>   # AES key for GitHub tokens at rest
```

- `WORKOS_APP_ORIGIN` is the exact trusted browser origin: `http` or `https`,
  with no credentials, path, query, or fragment. Catalog browser mutations fail
  closed when it is missing or malformed. Do not derive it from forwarded host
  headers.
- Mark the secret ones as **secrets** in Coolify.
- Add [persistent storage](https://coolify.io/docs/knowledge-base/persistent-storage)
  mounted at `/data` for repositories/worktrees, attachments, and agent state.
  The Postgres database is a separate resource.
- ⚠ `WORKOS_ENC_KEY` must be **stable forever** — changing it makes stored GitHub
  tokens undecryptable (users just re-login, but don't rotate it casually).

## 4. Point the GitHub App at production

In your operator-owned GitHub App, add this callback URL:

```
https://workos.example.com/auth/github/callback
```

(Keep the localhost one for dev.) Homepage URL → `https://workos.example.com`.

## 5. Deploy

- Coolify → **Deploy**. First build compiles the Rust server (slow, one-time).
- Watch logs for `Listening on` and the migration lines (`… has been applied`).
- Open `https://workos.example.com` → it redirects to GitHub login → authorize →
  you land in the workspace. Confirm your org/user rows exist (Coolify DB console:
  `SELECT login FROM orgs; SELECT login, org_id FROM users;`).

## 6. Security (non-negotiable for an internet-exposed agent runner)

- Everyone who logs in currently shares the app token (v1 bridge). Real per-user
  API auth + per-org agent **execution isolation** is Phase 2/4 — do NOT onboard a
  second, untrusted org until that lands (org A's agent could reach org B's files).
- Keep the Postgres resource internal-only (Coolify default).
- Keep `GITHUB_CLIENT_SECRET` out of git and rotate it through your operator's
  normal secret-management process.
- Consider putting the whole thing behind a VPN (Tailscale) for the internal pilot
  instead of the public internet.

## Upgrades

Deploy from your reviewed release branch; migrations run automatically on boot.
