# Neev public website

Static public homepage for https://neev.so. The application remains at https://app.neev.so.

## Design and content

Type.com was reviewed for spacious typography, restrained color, product-led storytelling, and short sections. Neev uses original copy, CSS-built sample product visuals, and an animated example workspace. No customer endorsements or unimplemented integrations are claimed. The hero supporting sentence is deliberately short, following user feedback.

The interactive example is illustrative; it does not run an agent. Preview tabs support arrows/Home/End. Animation has a pause control, stops offscreen/in background tabs, and respects reduced-motion preferences. Mobile navigation supports Escape. No external fonts, images, JavaScript libraries, analytics, or API requests.

## Local preview

Run `python3 -m http.server 4179 --bind 127.0.0.1 --directory marketing` from the repository root. The six public files total approximately 50 KB uncompressed.

## Deployment

Six public assets are copied to `/tmp/neev-website-upload` on the existing host. `scripts/operations/publish_neev_website.py <index-html-sha256>` creates a versioned directory under `/var/lib/neev-proxy/data/website/releases`, backs up the existing Caddyfile, validates a candidate, and gracefully reloads Caddy with SIGUSR1. Caddy 2.11 supports this for a process started with `run --config`, without API configuration changes: https://caddyserver.com/docs/command-line.

Only the existing apex redirect block is replaced. App proxy routes remain unchanged; old apex workspace/settings/login links still redirect to the app. HTTP and www redirect to canonical HTTPS apex. The server root contains only public files; the Caddy validation file sits outside it. A restrictive CSP and compression apply to the public homepage.

On verification failure the publisher restores the old config and reloads it. Manual rollback: restore the timestamped `/etc/neev-proxy/Caddyfile.before-website-*` file **in place** (the running container bind-mounts the inode), validate, and send USR1. Keep production backups and app volumes intact.

## Release verification — 2026-09-29 IST

- Release directory: `20260928T185445Z` (UTC).
- HTML SHA-256: `fb226eb1390ea6da62f09aec48040fa63d6b6af4b6bcfec6ac043371b9913d0a`.
- All six deployed assets compared byte-for-byte with source.
- HTML IDs/local references and JavaScript syntax validated.
- Desktop and 390px mobile reviewed; mobile document width matches viewport.
- Preview click/keyboard navigation, animation pause/play, menu open/Escape tested.
- Live HTTPS homepage and existing app health verified; application container unchanged.
- Only observed browser console errors came from an unrelated installed capture extension.
- No Git push performed. Existing app CSS/workspace edits were excluded from the website commit.


## Multiplayer and knowledge update — 2026-09-29 IST

- Multiplayer is now the initial product preview, with an illustrative shared document, collaborator presence, moving cursors, and team conversation. Animation pause/play and reduced-motion controls remain in place.
- Added original CSS/vector artwork to document and generated-output visuals, plus a layered organization knowledge illustration.
- Added a Team knowledge preview and FAQ grounded in current shared documents/files/skills capabilities. Does not claim automatic indexing or access to all organization content.
- Claude and Codex labeled available; additional agents explicitly coming soon.
- Kept the approved hero and colors; the supporting line stays short.
- Release `20260928T190939Z`; HTML SHA-256 `f354ce2be9ac13359c4845ead37a2fa5e51f59b4666d52d2a5f0dfacbbdb170d`.
- Verified all six deployed assets, initial tab state, keyboard Home/End switching, animation controls, and 390px layout without horizontal overflow. Existing app health/container unchanged.
