#!/usr/bin/env bash
#
# Build the Neev document engine frontend: La Suite Docs v5.7.0, patched to be
# served under a path prefix and embedded without its own app chrome.
#
# Fetches the pinned tarball, verifies its hash, applies the patch series,
# builds the static export, checks the output against the routes contract and
# packages it with a per-file hash manifest.
#
#   ./build.sh [work-dir]      default: /tmp/neev-lasuite-build
#
# Requires node 22 and yarn 1 on PATH. On this Mac:
#   export PATH=/opt/homebrew/opt/node@22/bin:$PATH
#
# DISK. Budget ~12 GB free, and do not run this on a host that has less.
# Measured, not estimated:
#   yarn fetch cache   ~7.3 GB   expanded during install, deleted at the end
#   node_modules       ~2.0 GB   kept, it is the install
#   .next build cache  ~1-2 GB   deleted at the end
#   out/                 45 MB   the artifact
#   tarball              24 MB
# Everything lives under the work directory, including the yarn cache, so
# removing that one directory reclaims all of it. An earlier version of this
# script claimed ~4 GB was enough; that ignored the fetch cache and it filled
# the disk.
#
# TIME. The install is ~2.5 min and the build ~1.5 min, so budget 10+ minutes
# in total and do not run this under a short foreground timeout — a previous run
# was SIGTERMed at the 10 minute mark, mid-install.
#
# REUSING AN EXISTING INSTALL. If $WORK/src/node_modules is already populated
# from a previous run, pass KEEP_INSTALL=1 to skip the install entirely. That
# turns a 4 minute run into a 1.5 minute one and never expands the fetch cache.
# It is only safe when the patch series has not changed a dependency, which so
# far it never has — no patch here touches package.json or yarn.lock.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${1:-/tmp/neev-lasuite-build}"
KEEP_INSTALL="${KEEP_INSTALL:-0}"

# shellcheck source=upstream.pin
source "$HERE/upstream.pin"

SRC="$WORK/src"
APP="$SRC/apps/impress"
TARBALL="$WORK/$(basename "$UPSTREAM_TARBALL")"

mkdir -p "$WORK"

say() { printf '\n== %s\n' "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }

say "Fetching $UPSTREAM_TAG"
if [ ! -f "$TARBALL" ]; then
  curl -fsSL --max-time 600 -o "$TARBALL" "$UPSTREAM_TARBALL"
fi

say "Verifying the pin"
# Refusing to build on a hash mismatch is the entire point of pinning: the tag
# could have been moved, or the download truncated.
echo "$UPSTREAM_TARBALL_SHA256  $TARBALL" | shasum -a 256 -c -

say "Extracting the frontend workspace"
STAGE="$WORK/.extract"
rm -rf "$STAGE"
mkdir -p "$STAGE"
tar xzf "$TARBALL" -C "$STAGE" "$UPSTREAM_SUBTREE"
if [ "$KEEP_INSTALL" = "1" ] && [ -d "$SRC/node_modules" ]; then
  # Keep node_modules, replace everything else with pristine sources.
  find "$SRC" -mindepth 1 -maxdepth 1 ! -name node_modules -exec rm -rf {} +
else
  rm -rf "$SRC"
fi
mkdir -p "$SRC"
(cd "$STAGE/$UPSTREAM_SUBTREE" && tar cf - .) | (cd "$SRC" && tar xf -)
rm -rf "$STAGE"

say "Applying the patch series"
for p in "$HERE"/patches/*.patch; do
  echo "  $(basename "$p")"
  patch -p1 -s -d "$SRC" < "$p"
done

say "Writing the embedded environment"
cp "$HERE/env/embed.env" "$APP/.env.local"

if [ "$KEEP_INSTALL" = "1" ] && [ -d "$SRC/node_modules" ]; then
  say "Reusing the existing install (KEEP_INSTALL=1)"
else
  say "Installing dependencies"
  # Kept inside the work directory rather than the global cache. Yarn 1 copies
  # out of its cache into node_modules, so a shared global cache means paying
  # for the same ~7 GB twice and leaving half of it behind afterwards.
  export YARN_CACHE_FOLDER="$WORK/yarn-cache"
  (cd "$SRC" && yarn install --frozen-lockfile --non-interactive)
fi

say "Building the static export"
# prebuild copies the Apple emoji images into public/. They are only referenced
# by the AGPL export path, which a PUBLISH_AS_MIT=true build never loads, but it
# is upstream's build step and skipping it is not ours to decide.
(cd "$APP" && node scripts/prebuild.mjs && npx next build --webpack)

say "Checking the output against the routes contract"
[ -f "$APP/out/docs/[id]/index.html" ] || fail "no docs/[id]/index.html — the UUID rewrite has nothing to serve"
if [ -e "$APP/out/service-worker.js" ]; then fail "a service worker was emitted"; fi
grep -q '/document-engine/_next/' "$APP/out/docs/[id]/index.html" || fail "assets are not prefixed"
if grep -rqa 'document-engine/document-engine' "$APP/out"; then fail "prefix applied twice"; fi
grep -rqa '/document-engine/collaboration/ws' "$APP/out/_next" || fail "collaboration socket is not prefixed"
grep -rqa 'window.location.origin}/document-engine' "$APP/out/_next" || fail "the API base is not composed from the runtime origin"
echo "  ok"

say "Packaging"
STAMP="$(cd "$APP/out" && find . -type f | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256 | cut -c1-12)"
ART="$WORK/neev-document-engine-frontend-$UPSTREAM_TAG-$STAMP.tar.gz"
MANIFEST="$WORK/neev-document-engine-frontend-$UPSTREAM_TAG-$STAMP.sha256"

# A per-file manifest, not a claim of bit-reproducibility. Two runs of this
# script do NOT produce byte-identical output: upstream derives the Next build
# id from random bytes, so chunk names and their contents differ every time.
# The manifest is what lets you verify that the bytes you deployed are the bytes
# that were built and checked here.
(cd "$APP/out" && find . -type f | LC_ALL=C sort | xargs shasum -a 256) > "$MANIFEST"

COPYFILE_DISABLE=1 tar --no-xattrs --uid 0 --gid 0 --numeric-owner \
  -C "$APP/out" -cf - . | gzip -9 -n > "$ART"

say "Reclaiming the build cache"
rm -rf "$APP/.next" "$WORK/yarn-cache"

say "Done"
shasum -a 256 "$ART"
echo "manifest: $MANIFEST ($(wc -l < "$MANIFEST") files)"
du -sh "$APP/out" "$ART"
