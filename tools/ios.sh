#!/usr/bin/env bash
# The iPhone app (needs a Mac with Xcode; CI runs it in .github/workflows/ios.yml). It is the store
# build: --no-default-features --features play, so no payments, no uncensored or roleplay models, no
# Tor, and Code Mode is off (docs/ios.md).
#
#   tools/ios.sh build      simulator app -> target/ios/Obscurify.app (ad-hoc signed, simulator only)
#   tools/ios.sh sim        build, boot a simulator, install, launch, screenshot -> target/ios/
#
# Not signed for a device or the App Store yet: that needs the Apple developer account (docs/ios.md).
# Test builds only: OBSCURIFY_API=https://... points the app at another server. Never ship one.
set -euo pipefail
cd "$(dirname "$0")/.."
id=$(sed -n '/^\[ios\]/,/^\[/s/^identifier = "\(.*\)"/\1/p' Dioxus.toml)
out=target/ios
triple=${IOS_TARGET:-aarch64-apple-ios-sim}

build() {
  dx build --platform ios --release --target "$triple" --no-default-features --features play
  app=$(find target/dx/obscurify/release/ios -name '*.app' -type d -prune | head -1)
  [ -n "$app" ] || { echo "dx made no .app" >&2; exit 1; }
  rm -rf $out && mkdir -p $out && cp -R "$app" $out/Obscurify.app
  # The simulator runs ad-hoc signed apps; a device or the store needs the real signing (docs/ios.md).
  codesign --force --sign - --timestamp=none $out/Obscurify.app
  plutil -p $out/Obscurify.app/Info.plist
  echo "built $(pwd)/$out/Obscurify.app ($id)"
}

sim() {
  build
  # The newest iPhone simulator Xcode has.
  udid=${SIM_UDID:-$(xcrun simctl list devices available -j | python3 -c '
import json, sys
d = json.load(sys.stdin)["devices"]
phones = [(rt, x) for rt, xs in d.items() if "iOS" in rt for x in xs if x["name"].startswith("iPhone")]
rt, x = max(phones, key=lambda p: (p[0], p[1]["name"]))
print(x["udid"])')}
  xcrun simctl list devices | grep "$udid"
  xcrun simctl boot "$udid" 2>/dev/null || true
  xcrun simctl bootstatus "$udid" -b
  xcrun simctl install "$udid" $out/Obscurify.app
  xcrun simctl launch "$udid" "$id"
  sleep ${WAIT:-30}
  xcrun simctl io "$udid" screenshot $out/screenshot.png
  # Still running? (A crash leaves the home screen in the screenshot.)
  xcrun simctl spawn "$udid" launchctl list | grep -i "$id" || { echo "the app isn't running" >&2; exit 1; }
  echo "screenshot $(pwd)/$out/screenshot.png"
}

case "${1:-build}" in
  build) build ;;
  sim) sim ;;
  *) echo "usage: $0 build|sim" >&2; exit 2 ;;
esac
