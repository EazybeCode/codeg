# Attachment preview transfer — 2026-09-28

The previous release prevented polling from restarting downloads, but still
showed a generic loading message until the complete HTML arrived. V106 is
22,696,271 bytes. Production Caddy compression only covered app/editor assets,
not authenticated attachment responses.

Changes:
- Stream HTML reads with received-byte progress in the preview.
- Abort after 30 seconds without response/body progress; expose an explicit retry.
- Keep full-file preview and existing HTML sandbox/authorization boundaries.
- Extend the Caddy compression path matcher with `/api/neev/attachments/*`;
  MIME filtering remains Caddy's default. No caching or access-policy changes.
- All ten locales include progress/retry labels.

Verification: regression for progress failed before change; 18 tests passed,
including stalled-request abort, retry, and preserving downloads across polling.
Caddy candidate validated before restart; health passed after restart. Config
backup retained under /etc/neev-proxy/Caddyfile.before-attachment-compression-*.
Local gzip measurement of V106: 15,110,342 bytes versus 22,696,271 original.
This is a compression estimate, not a measured end-to-end latency claim.

Production frontend: `neev-native-agent:preview-progress-20260928`; workspace SHA256
`acfbe82ad49a6358646f7dde5f259eede242e87248fd8d6c2dff79b42a8cbb68`.
Next production build/typecheck and ESLint passed. Release guard reported zero
active sign-ins/turns, took a verified backup, and checked unchanged backend/schema17.
Live browser showed V106 progress advancing from 0.1 MB to 15.5 MB.
The live V106 preview subsequently rendered; screenshot verified visible page
content in the modal. The working preview tab was left open for user review.
