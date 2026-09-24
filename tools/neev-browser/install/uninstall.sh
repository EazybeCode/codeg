#!/usr/bin/env bash
# Remove the Neev Browser native messaging host manifests.
#
# This removes only the manifests install.sh wrote. Your runs in
# ~/.neev/browser-runs are left alone — delete that directory yourself if you
# want the outputs gone.
set -euo pipefail

HOST_NAME="com.neev.browser"

case "$(uname -s)" in
  Darwin)
    SUPPORT="$HOME/Library/Application Support"
    DIRS=(
      "$SUPPORT/Google/Chrome/NativeMessagingHosts"
      "$SUPPORT/Google/Chrome Beta/NativeMessagingHosts"
      "$SUPPORT/Google/Chrome Canary/NativeMessagingHosts"
      "$SUPPORT/Chromium/NativeMessagingHosts"
      "$SUPPORT/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    )
    ;;
  Linux)
    DIRS=(
      "$HOME/.config/google-chrome/NativeMessagingHosts"
      "$HOME/.config/chromium/NativeMessagingHosts"
      "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts"
    )
    ;;
  *)
    echo "uninstall.sh: unsupported platform $(uname -s)" >&2
    exit 1
    ;;
esac

removed=0
for dir in "${DIRS[@]}"; do
  manifest="$dir/$HOST_NAME.json"
  if [ -f "$manifest" ]; then
    rm -f "$manifest"
    echo "removed $manifest"
    removed=$((removed + 1))
  fi
done

if [ "$removed" -eq 0 ]; then
  echo "No $HOST_NAME manifests were found; nothing to remove."
fi

echo
echo "Kept: $HOME/.neev/browser-runs (your run outputs)."
echo "Remove the extension itself at chrome://extensions."
