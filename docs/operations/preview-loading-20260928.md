# Preview loading fix — 2026-09-28

Chat refreshes return new attachment objects. FilePreview depended on that object
identity, aborting and restarting pending HTML/text downloads on every refresh.
Memoize the four attachment metadata fields so equivalent refreshes preserve the
request. Different attachment keys still remount the preview.

Regression test failed before the fix (original signal aborted) and passed after;
all six file-action tests passed. ESLint and production Next build passed.

Deployed frontend-only image `neev-native-agent:preview-loading-20260928`.
Verified live workspace SHA256:
`4b55f0a9c9ae25d897ac599dd8b6688733b7b0cbdf72da6774265329ea708cb5`.
Backend unchanged, schema 17. Fresh database backup verified before deployment;
zero active sign-ins or agent turns at release time.

This fixes repeated preview downloads, not general document-editor startup.

Live browser verification: the 21.6 MB V106 attachment completed and rendered
Conversation Management inside the sandboxed preview. Large-file transfer still
adds latency; this release does not claim instant previews or faster editor boot.
