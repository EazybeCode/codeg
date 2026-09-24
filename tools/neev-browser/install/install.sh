#!/usr/bin/env bash
# Register the Neev Browser native messaging host for one extension ID.
#
# This writes exactly one kind of file — a native messaging host manifest —
# into the browser profile directories that already exist. It does not install
# the extension, does not grant any permission, and never touches your Claude
# or Codex configuration.
#
#   usage: install/install.sh <extension-id>
#
# Load the unpacked extension at chrome://extensions first, then copy the ID
# Chrome assigns it and pass it here.
set -euo pipefail

HOST_NAME="com.neev.browser"
HERE=$(cd "$(dirname "$0")" && pwd)
HOST_BIN="$(cd "$HERE/../companion" && pwd)/neev-browser-host"

die() { echo "install.sh: $*" >&2; exit 1; }

EXT_ID="${1:-}"
[ -n "$EXT_ID" ] || die "give the extension ID as the only argument.
Load tools/neev-browser/extension as an unpacked extension at
chrome://extensions, then copy the ID it is assigned."

if ! printf '%s' "$EXT_ID" | grep -Eq '^[a-p]{32}$'; then
  die "'$EXT_ID' is not a Chrome extension ID (32 letters, a through p)."
fi

command -v python3 >/dev/null 2>&1 || die "python3 is required but was not found."
[ -x "$HOST_BIN" ] || die "the host script is missing or not executable: $HOST_BIN
Run: chmod +x '$HOST_BIN'"

case "$(uname -s)" in
  Darwin)
    SUPPORT="$HOME/Library/Application Support"
    CANDIDATES=(
      "$SUPPORT/Google/Chrome/NativeMessagingHosts"
      "$SUPPORT/Google/Chrome Beta/NativeMessagingHosts"
      "$SUPPORT/Google/Chrome Canary/NativeMessagingHosts"
      "$SUPPORT/Chromium/NativeMessagingHosts"
      "$SUPPORT/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    )
    PRIMARY="$SUPPORT/Google/Chrome/NativeMessagingHosts"
    ;;
  Linux)
    CANDIDATES=(
      "$HOME/.config/google-chrome/NativeMessagingHosts"
      "$HOME/.config/chromium/NativeMessagingHosts"
      "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    )
    PRIMARY="$HOME/.config/google-chrome/NativeMessagingHosts"
    ;;
  *)
    die "unsupported platform $(uname -s). This installer covers macOS and Linux."
    ;;
esac

# Write to the primary profile plus any other browser profile already present.
TARGETS=("$PRIMARY")
for dir in "${CANDIDATES[@]}"; do
  [ "$dir" = "$PRIMARY" ] && continue
  parent=$(dirname "$dir")
  if [ -d "$parent" ]; then TARGETS+=("$dir"); fi
done

echo "Neev Browser native host"
echo "  host script : $HOST_BIN"
echo "  extension   : $EXT_ID"
echo "  manifests   :"
for dir in "${TARGETS[@]}"; do
  echo "      $dir/$HOST_NAME.json"
done
echo

MANIFEST=$(cat <<JSON
{
  "name": "$HOST_NAME",
  "description": "Neev Browser companion",
  "path": "$HOST_BIN",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXT_ID/"]
}
JSON
)

for dir in "${TARGETS[@]}"; do
  mkdir -p "$dir"
  printf '%s\n' "$MANIFEST" > "$dir/$HOST_NAME.json"
  chmod 600 "$dir/$HOST_NAME.json"
  echo "wrote $dir/$HOST_NAME.json"
done

echo
echo "Done. Fully quit and reopen Chrome so it picks up the new host."
echo "Runs are kept in: $HOME/.neev/browser-runs"
