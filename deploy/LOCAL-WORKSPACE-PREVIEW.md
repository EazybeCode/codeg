# Local repository-backed workspace preview

This slice is an operator-controlled, trusted single-user preview. Team access,
public hosting, personal provider isolation, full legacy API authorization,
publication and wiki compilation are still release gates.

The server serves `out/` and all auth/API/WebSocket routes from one origin. Build
the frontend with Node 22 (`pnpm build`) and build the server from `src-tauri`
with `cargo build --release --no-default-features --bin codeg-server`.
The resource-constrained development checkout uses the additional cached setting
`--config 'profile.release.package.codeg.opt-level=0'`.

Launch `bash deploy/start-local-preview.sh` with these explicit environment variables:

- `WORKOS_OPERATOR_ENV`: existing operator secrets file; never commit it.
- `WORKOS_PREVIEW_DATABASE_URL`: an existing, isolated database, never `workos_local`.
- `WORKOS_PREVIEW_DATA_DIR`: absolute durable path outside the source/build cache.
- `WORKOS_PREVIEW_TOKEN`: localhost-only installation token.
- `WORKOS_PREVIEW_PORT`: optional port; defaults to 3095.

The launcher binds only 127.0.0.1, enables the trusted catalog runtime, and sets
both `WORKOS_APP_ORIGIN` and `GITHUB_CALLBACK_URL` to the same localhost port.
It never copies a database or stops existing processes. Do not run the older
operator startup script containing broad `pkill` commands. Before restarting a
preview, verify the exact listener PID, command, cwd and active ACP work, then
stop only that owned process.

Open `http://localhost:3095/login.html` and enter the chosen installation token.
An existing valid `workos_session` cookie is separately required for workspace
identity. Copied sessions verify continuity only, not fresh GitHub OAuth.
For a new sign-in, the operator-owned GitHub App may need the matching callback;
a local redirect with the correct `redirect_uri` does not prove GitHub accepts
the registration. No provider sign-in or prompt success is implied by installed
executables. Missing legacy folder paths remain unavailable without rebinding
or removing their history.

## Reusable browser acceptance

`scripts/tests/workspace-e2e.cjs` drives the actual static-exported app. Build the
opt-in, `cfg(test)`-only fixture with the same Rust profile:

```sh
cd src-tauri
cargo test --release --config 'profile.release.package.codeg.opt-level=0' \
  --no-default-features --lib workspace_browser_fixture_server --no-run
cd ..
WORKOS_E2E_TEST_BINARY=/absolute/path/to/the/printed/codeg_lib-test-executable \
  node scripts/tests/workspace-e2e.cjs
```

Set `PLAYWRIGHT_MODULE` to an already-installed Playwright module if needed.
The driver creates a distinct durable fixture directory under ignored task
scratch, uses port 3097, and stops only its own test process. Screenshots, SQLite
database, local Git remote/worktrees, native provider transcripts and diagnostic
logs stay there. `CLAUDE_CONFIG_DIR` points only at that fixture directory.
Only external GitHub repository/status, local Git transport and provider behavior
are substituted. Catalog/execution admission, cookies/origin/installation checks,
prompt routing, WebSocket events, lifecycle metadata persistence, Claude parsing,
Files handlers and the application UI are real. Postgres stores conversation
metadata/linkage; full transcript turns are loaded from native provider files.

Back up and persist those provider directories separately. For example, Claude
uses `CLAUDE_CONFIG_DIR` when configured and otherwise `~/.claude`; it does not
automatically put transcripts under `CODEG_DATA_DIR`. Durable Postgres and `/data`
alone therefore do not guarantee transcript or provider-config survival when a
container is replaced.

These tests do not verify real GitHub transport/OAuth, real model providers,
untrusted tenant isolation, publication, or public deployment.
