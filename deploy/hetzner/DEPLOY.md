# Deploy Eazybe WorkOS on Hetzner

One hosted instance = one org's workspace. TLS via Caddy. ~15 min + build time.

## 0. Before you start
- A Hetzner Cloud server (Ubuntu 24.04, **≥ CPX31: 4 vCPU / 8 GB** — the Rust build
  is heavy; smaller works if you build the image elsewhere and pull it).
- A domain you control (e.g. `workos.eazybe.com`) with a DNS **A record → server IP**.

## 1. Server prep
```bash
ssh root@<server-ip>
apt update && apt install -y git
curl -fsSL https://get.docker.com | sh          # Docker + compose plugin
```

## 2. Get the fork + configure
```bash
git clone -b eazybe-workos https://github.com/EazybeCode/codeg.git
cd codeg/deploy/hetzner
cp .env.example .env
openssl rand -hex 24        # paste as CODEG_TOKEN
nano .env                   # set ORG_SLUG, DOMAIN, CODEG_TOKEN, ANTHROPIC_AUTH_TOKEN / OPENAI_API_KEY
```

## 3. Firewall (only web ports open)
```bash
ufw allow OpenSSH && ufw allow 80 && ufw allow 443 && ufw --force enable
```

## 4. Launch
```bash
docker compose up -d --build     # first build compiles frontend + Rust server (slow, one-time)
docker compose logs -f workos    # watch for "Listening on"
```
Open **https://workos.eazybe.com** and log in with `CODEG_TOKEN`.

## 5. A second org
```bash
cp -r deploy/hetzner ../workos-acme && cd ../workos-acme
# edit .env: ORG_SLUG=acme, DOMAIN=workos.acme.com, a NEW CODEG_TOKEN, its own PROJECTS_DIR
docker compose up -d --build     # isolated stack: own container, data volume, token, domain
```
Each org is fully isolated (separate data volume + token + domain). This is how
"org-dependent" works today — per-org deployment, not in-app tenancy.

## 6. Upgrades
codeg's in-app "Software Update" only lives in the running container and is lost on
recreate. To upgrade permanently: `git pull`, then `docker compose up -d --build`.

## 7. Backups
Everything stateful is the `data` volume (sessions DB + uploads):
```bash
docker run --rm -v workos-eazybe_data:/data -v $PWD:/backup alpine \
  tar czf /backup/workos-data-$(date +%F).tar.gz -C /data .
```

## ⚠ Security — read this
You are exposing a server that **runs file-editing / shell-executing AI agents** to
the internet. Treat it seriously:
- **Strong `CODEG_TOKEN`** (never `eazybe-dev`) + **TLS** (Caddy does this) are the
  minimum. Add Caddy **basic-auth** (see Caddyfile) for a second gate.
- **Best option:** don't expose it publicly at all — put the server on a **Tailscale
  / WireGuard VPN** and skip ports 80/443 on the public interface. Only the org's
  people on the VPN reach it. Strongly preferred for an agent-runner.
- Agents can read anything in `/projects` and the container's env (incl. the API
  keys). Mount only repos the org should access; rotate keys; consider a dedicated
  low-privilege git token.
- codeg's shared-token auth has **no per-user identity** — everyone with the token
  is the same principal. Real accounts/roles are greenfield (decision 0005). Until
  then, the token is the whole security boundary — guard it like a password.
