# Deploy Eazybe WorkOS on Coolify + Hetzner + Postgres

Validated: all 46 migrations apply cleanly on Postgres 16, server boots on PG.
This is the multi-tenant (GitHub-login) deploy.

## 0. Provision
- **Hetzner** Cloud server, Ubuntu 24.04, **≥ CPX31 (4 vCPU / 8 GB)** — the Rust
  image build is heavy. (Or build the image elsewhere and let Coolify pull it.)
- Install **Coolify** on it: `curl -fsSL https://coolify.io/install.sh | bash`
  then open `http://<server-ip>:8000` and finish setup.
- A domain with a DNS **A record → server IP** (e.g. `workos.eazybe.com`).

## 1. Add the Postgres database (Coolify resource)
- Coolify → your project → **+ New** → **Database** → **PostgreSQL 16**.
- Create it. Coolify gives an **internal connection URL** like
  `postgres://<user>:<pass>@<service-name>:5432/<db>`. Copy it — that's your
  `DATABASE_URL`. (Internal-only; not exposed publicly. Good.)
- Enable Coolify's **scheduled backups** on this database.

## 2. Add the app (from your GitHub repo)
- Coolify → **+ New** → **Application** → **Public/Private Repository**.
- Repo: `EazybeCode/codeg`, branch: **`multitenancy`** (until merged to main).
- Build pack: **Dockerfile** (the repo's `Dockerfile` builds frontend + Rust server).
- Port: **3080**. Set your domain (`workos.eazybe.com`); Coolify auto-issues TLS.

## 3. Environment variables (Coolify → the app → Environment)
```
DATABASE_URL      = <the Postgres internal URL from step 1>
CODEG_TOKEN       = <openssl rand -hex 24>      # break-glass/admin token
CODEG_HOST        = 0.0.0.0
CODEG_PORT        = 3080
CODEG_STATIC_DIR  = /app/web                    # (Dockerfile already sets this)
CODEG_DATA_DIR    = /data                       # uploads etc. (mount a volume)
GITHUB_CLIENT_ID     = Iv23lixFMTeBFIuDHo02
GITHUB_CLIENT_SECRET = <rotated secret>         # ROTATE the exposed one first
GITHUB_CALLBACK_URL  = https://workos.eazybe.com/auth/github/callback
WORKOS_ENC_KEY       = <openssl rand -hex 32>   # AES key for GitHub tokens at rest
```
- Mark the secret ones as **secrets** in Coolify.
- Add a **persistent volume** mounted at `/data` (for uploads; the DB is separate).
- ⚠ `WORKOS_ENC_KEY` must be **stable forever** — changing it makes stored GitHub
  tokens undecryptable (users just re-login, but don't rotate it casually).

## 4. Point the GitHub App at production
GitHub → **EazyWorkOS** app → **Callback URLs** → add:
```
https://workos.eazybe.com/auth/github/callback
```
(Keep the localhost one for dev.) Homepage URL → `https://workos.eazybe.com`.

## 5. Deploy
- Coolify → **Deploy**. First build compiles the Rust server (slow, one-time).
- Watch logs for `Listening on` and the migration lines (`… has been applied`).
- Open `https://workos.eazybe.com` → it redirects to GitHub login → authorize →
  you land in the workspace. Confirm your org/user rows exist (Coolify DB console:
  `SELECT login FROM orgs; SELECT login, org_id FROM users;`).

## 6. Security (non-negotiable for an internet-exposed agent runner)
- Everyone who logs in currently shares the app token (v1 bridge). Real per-user
  API auth + per-org agent **execution isolation** is Phase 2/4 — do NOT onboard a
  second, untrusted org until that lands (org A's agent could reach org B's files).
- Keep the Postgres resource internal-only (Coolify default).
- Rotate `GITHUB_CLIENT_SECRET` (the earlier one was exposed) and keep it out of git.
- Consider putting the whole thing behind a VPN (Tailscale) for the Eazybe pilot
  instead of the public internet.

## Upgrades
Push to `multitenancy` (or main) → Coolify auto-redeploys (rebuilds the image).
Migrations run automatically on boot.
