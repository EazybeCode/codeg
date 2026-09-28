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
