# Eazybe WorkOS (codeg fork)

Internal fork of [codeg](https://github.com/xintaofei/codeg) (Apache-2.0), rebranded
**Eazybe WorkOS**, as the base for our shared AI coding workspace. We keep codeg's
polished UI + agent runner (Claude/Codex, git worktrees, sessions) and add our own
git-native shared-memory / collaboration layer on top.

Fork: `github.com/EazybeCode/codeg`

## Run (from source, already built)

```bash
~/eazybe-codeg/run-workos.sh                    # local → http://localhost:3080
CODEG_HOST=0.0.0.0 ~/eazybe-codeg/run-workos.sh # team → http://<your-ip>:3080
```

Access token: `eazybe-dev` (set `CODEG_TOKEN` to change). Data dir: `~/eazybe-codeg/data`.

## Rebuild after changing code

Toolchain (installed once): Rust (rustup), Node 22 (`/opt/homebrew/opt/node@22/bin`),
pnpm 11.9, openssl@3 + pkg-config.

```bash
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH"
cd ~/eazybe-codeg/codeg-src

# frontend (Next.js) — after UI/branding changes:
pnpm install            # first time / dep changes
pnpm build              # outputs static site to ./out

# backend (Rust server) — after src-tauri changes:
export OPENSSL_DIR="$(brew --prefix openssl@3)"
export PKG_CONFIG_PATH="$(brew --prefix openssl@3)/lib/pkgconfig"
cd src-tauri && cargo build --release --bin codeg-server --no-default-features
```

Dev loop (hot reload) instead of full builds:
```bash
pnpm dev            # Next dev server (frontend)
pnpm server:dev     # cargo run codeg-server (backend)
```

## What we've branded so far (surgical — no functional renames)
- `src/app/layout.tsx` — browser title → "Eazybe WorkOS"
- `src/components/layout/app-boot-loading.tsx` — boot wordmark → "Eazybe WorkOS"

The 900+ internal `codeg` identifiers (env vars `CODEG_*`, storage keys, API paths)
are deliberately left untouched — they're not user-visible and renaming them would
break the app.

## Next (planned)
- Deeper branding/theme (accent color, icon) — pending design input
- Git-native shared-memory layer (the WorkOS/eazybe-wiki work) wired in as a module
- Assess lifting select UI components from `meltylabs/chorus` (MIT)
