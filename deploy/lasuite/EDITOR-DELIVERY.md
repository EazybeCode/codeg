# Editor static delivery

Include `editor-delivery.caddy` inside the public app site block before its
`reverse_proxy`. Validate the complete configuration using the deployed Caddy
image before reloading it. Keep a copy of the previous configuration.

The editor is a separate static export under `/document-engine`. The existing
native `/_next/static/*` compression matcher does not cover it. Its main bundle
was 1,504,479 bytes uncompressed (462,562 gzip), and the native artifact handler
returns `no-cache` without validators. Reopening an iframe therefore downloads
the bundles again.

This snippet compresses editor static assets and permits private browser caching
only for successful, hash-named static assets. HTML, document content, API,
attachments and collaboration retain their existing authorization/cache policy.
Unauthorized and missing assets do not become cacheable. Filename hashes change
when the build changes, so a new deployment fetches its new bundles.

Validation on the deployed Caddy image (2026-09-23): six loopback HTTP checks
passed: hash-named 200 compressed/cacheable; 401 and 404 remain no-store;
HTML and API remain no-store; unhashed static files compressed without immutable
caching. Complete production configuration validates. This is a delivery fix,
not an authorization cache.

Syntax reference: https://caddyserver.com/docs/caddyfile/directives/header
