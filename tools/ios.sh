#!/usr/bin/env bash
# The iPhone app (needs a Mac with Xcode; CI runs it in .github/workflows/ios.yml). It is the store
# build: --no-default-features --features play, so no payments, no uncensored or roleplay models and
# no Tor (docs/ios.md).
#
#   tools/ios.sh build      simulator app -> target/ios/Obscurify.app (ad-hoc signed, simulator only)
#   tools/ios.sh device     iPhone app -> target/ios-device/Obscurify.app and Obscurify.ipa (ad-hoc
#                           signed: re-sign it for a device, e.g. with xtool, or for the App Store)
#   tools/ios.sh sim        build, boot a simulator, install, launch, screenshot -> target/ios/
#   tools/ios.sh e2e        the e2e build in an iPhone Pro Max simulator, driven by e2e/ios/driver.js
#                           against production; screenshots and logs -> target/ios/e2e/ (E2E_SIGNUP=1
#                           also makes an account, and deletes it from the app at the end)
#
# Every build gets what an App Store upload needs that dx doesn't make (docs/ios.md, "Bundle
# contents"): the app icon, the launch screen, the privacy manifest and the Info.plist keys, and is
# checked for them.
# Test builds only: OBSCURIFY_API=https://... points the app at another server, E2E=1 builds in the
# e2e driver. Never ship either: `device` refuses E2E.
set -euo pipefail
cd "$(dirname "$0")/.."
id=$(sed -n '/^\[ios\]/,/^\[/s/^identifier = "\(.*\)"/\1/p' Dioxus.toml)
min_ios=$(sed -n '/^\[ios\]/,/^\[/s/^deployment_target = "\(.*\)"/\1/p' Dioxus.toml)
version_code=$(sed -n 's/^version-code = \([0-9]*\)/\1/p' Cargo.toml)
# rustc puts this in the binary (LC_BUILD_VERSION minos); MinimumOSVersion says the same.
export IPHONEOS_DEPLOYMENT_TARGET=$min_ios

build() { # triple, out dir
  local triple=$1 out=$2 sdk features=play
  case "$triple" in *-sim) sdk=iphonesimulator ;; *) sdk=iphoneos ;; esac
  [ "${E2E:-}" = 1 ] && features=play,e2e
  dx build --platform ios --release --target "$triple" --no-default-features --features "$features"
  local app
  app=$(find target/dx/obscurify/release/ios -name '*.app' -type d -prune | head -1)
  [ -n "$app" ] || { echo "dx made no .app" >&2; exit 1; }
  rm -rf "$out" && mkdir -p "$out" && cp -R "$app" "$out/Obscurify.app"
  bundle "$out/Obscurify.app" "$sdk" "$out"
  # The simulator runs ad-hoc signed apps; a device or the store needs the real signing (docs/ios.md).
  codesign --force --sign - --timestamp=none "$out/Obscurify.app"
  check "$out/Obscurify.app" "$sdk"
  plutil -p "$out/Obscurify.app/Info.plist"
  echo "built $(pwd)/$out/Obscurify.app ($id, $sdk)"
}

# What an App Store upload needs that dx 0.7 doesn't make.
bundle() { # app, sdk, scratch dir
  local app=$1 sdk=$2 tmp=$3 plist=$1/Info.plist platform
  rm -rf "$app/_CodeSignature"
  # The icon: one 1024 px image; actool makes the home screen sizes and Assets.car.
  xcrun actool ios/Assets.xcassets --compile "$app" --platform "$sdk" --minimum-deployment-target "$min_ios" \
    --app-icon AppIcon --target-device iphone --output-partial-info-plist "$tmp/icons.plist" \
    --output-format human-readable-text --errors --warnings --notices
  /usr/libexec/PlistBuddy -c "Merge $tmp/icons.plist" "$plist"
  xcrun ibtool ios/LaunchScreen.storyboard --compile "$app/LaunchScreen.storyboardc" --minimum-deployment-target "$min_ios" \
    --target-device iphone --output-format human-readable-text --errors --warnings
  cp ios/PrivacyInfo.xcprivacy "$app/"
  plutil -replace MinimumOSVersion -string "$min_ios" "$plist"
  # Every upload needs a higher build number: the Android version code.
  plutil -replace CFBundleVersion -string "$version_code" "$plist"
  # It ships its own standard crypto (TLS, AES-GCM, blind RSA): not only the OS's (docs/ios.md).
  plutil -replace ITSAppUsesNonExemptEncryption -bool YES "$plist"
  # iPhone only, portrait: no iPad screenshots or layouts to keep up.
  plutil -replace UIDeviceFamily -json '[1]' "$plist"
  plutil -replace UISupportedInterfaceOrientations -json '["UIInterfaceOrientationPortrait"]' "$plist"
  plutil -remove 'UISupportedInterfaceOrientations~ipad' "$plist" 2>/dev/null || true
  plutil -remove UISupportsTrueScreenSizeOnMac "$plist" 2>/dev/null || true
  # What Xcode writes about the build; App Store validation reads them.
  case "$sdk" in iphonesimulator) platform=iPhoneSimulator ;; *) platform=iPhoneOS ;; esac
  local sdkver xcode xbuild
  sdkver=$(xcrun --sdk "$sdk" --show-sdk-version)
  xcode=$(xcodebuild -version | sed -n 's/^Xcode \([0-9.]*\)/\1/p')
  xbuild=$(xcodebuild -version | sed -n 's/^Build version \(.*\)/\1/p')
  plutil -replace CFBundleSupportedPlatforms -json "[\"$platform\"]" "$plist"
  plutil -replace DTPlatformName -string "$sdk" "$plist"
  plutil -replace DTPlatformVersion -string "$sdkver" "$plist"
  plutil -replace DTSDKName -string "$sdk$sdkver" "$plist"
  plutil -replace DTSDKBuild -string "$(xcrun --sdk "$sdk" --show-sdk-build-version)" "$plist"
  plutil -replace DTPlatformBuild -string "$(xcrun --sdk "$sdk" --show-sdk-build-version)" "$plist"
  plutil -replace DTXcode -string "$(echo "$xcode" | awk -F. '{printf "%02d%d%d", $1, $2, $3}')" "$plist"
  plutil -replace DTXcodeBuild -string "$xbuild" "$plist"
  plutil -replace DTCompiler -string com.apple.compilers.llvm.clang.1_0 "$plist"
  plutil -replace BuildMachineOSBuild -string "$(sw_vers -buildVersion)" "$plist"
}

# The bundle has what the upload needs, and the binary only uses required-reason APIs the privacy
# manifest declares.
check() { # app, sdk
  local app=$1 sdk=$2 bin=$1/obscurify plist=$1/Info.plist bad=0
  for f in Assets.car AppIcon60x60@2x.png LaunchScreen.storyboardc PrivacyInfo.xcprivacy; do
    [ -e "$app/$f" ] || { echo "missing in the bundle: $f" >&2; bad=1; }
  done
  for k in MinimumOSVersion CFBundleVersion CFBundleShortVersionString ITSAppUsesNonExemptEncryption CFBundleIcons UILaunchStoryboardName DTPlatformName; do
    plutil -extract "$k" raw "$plist" >/dev/null 2>&1 || plutil -extract "$k" xml1 -o - "$plist" >/dev/null 2>&1 || { echo "missing in Info.plist: $k" >&2; bad=1; }
  done
  [ "$(plutil -extract CFBundleVersion raw "$plist")" = "$version_code" ] || { echo "CFBundleVersion isn't $version_code" >&2; bad=1; }
  local minos
  minos=$(xcrun vtool -show-build "$bin" | awk '/minos/ {print $2; exit}')
  echo "binary minos $minos, MinimumOSVersion $(plutil -extract MinimumOSVersion raw "$plist")"
  [ "$minos" = "$min_ios" ] || { echo "the binary's minos ($minos) isn't $min_ios" >&2; bad=1; }
  xcrun otool -L "$bin"
  # Apple's required-reason APIs (developer.apple.com/documentation/bundleresources/describing-use-of-required-reason-api).
  # Declared in ios/PrivacyInfo.xcprivacy: file timestamps (stat and friends, C617.1). Any other is a new
  # declaration to make.
  local imports
  imports=$(xcrun nm -u "$bin" | sed 's/^ *U *//')
  echo "required-reason imports:"; echo "$imports" | grep -xE '_(f?stat|lstat|fstatat|f?getattrlist|getattrlistat|getattrlistbulk|setattrlist|setattrlistbulk|statfs|fstatfs|statvfs|fstatvfs|mach_absolute_time)' || true
  local undeclared
  undeclared=$(echo "$imports" | grep -xE '_(f?getattrlist|getattrlistat|getattrlistbulk|setattrlist|setattrlistbulk|statfs|fstatfs|statvfs|fstatvfs|mach_absolute_time)' || true)
  [ -z "$undeclared" ] || { echo "required-reason APIs not in PrivacyInfo.xcprivacy: $undeclared" >&2; bad=1; }
  if strings "$bin" | grep -qE 'NSUserDefaults|systemUptime|activeInputModes|volumeAvailableCapacity'; then
    echo "the binary names a required-reason API PrivacyInfo.xcprivacy doesn't declare" >&2; bad=1
  fi
  # The e2e driver never goes into a device build.
  if [ "$sdk" = iphoneos ] && strings "$bin" | grep -q OBSCURIFY_E2E_DRIVER; then echo "the e2e driver is in a device build" >&2; bad=1; fi
  [ "$bad" = 0 ] || exit 1
  echo "bundle checked: $app"
}

# The newest simulator whose name matches $1 (a regex), else the newest iPhone.
simulator() {
  xcrun simctl list devices available -j | python3 -c '
import json, re, sys
want = sys.argv[1]
d = json.load(sys.stdin)["devices"]
phones = [(rt, x) for rt, xs in d.items() if "iOS" in rt for x in xs if x["name"].startswith("iPhone")]
pick = [p for p in phones if re.search(want, p[1]["name"])] or phones
rt, x = max(pick, key=lambda p: (p[0], p[1]["name"]))
print(x["udid"])' "${1:-.}"
}

boot() { # udid
  xcrun simctl list devices | grep "$1"
  xcrun simctl boot "$1" 2>/dev/null || true
  xcrun simctl bootstatus "$1" -b
}

sim() {
  build "${IOS_TARGET:-aarch64-apple-ios-sim}" target/ios
  local udid=${SIM_UDID:-$(simulator "${SIM_NAME:-.}")}
  boot "$udid"
  xcrun simctl install "$udid" target/ios/Obscurify.app
  # The app's own output (ios.rs logs what the native side does) goes to files next to the screenshot.
  xcrun simctl launch --terminate-running-process --stdout="$(pwd)/target/ios/stdout.log" --stderr="$(pwd)/target/ios/stderr.log" "$udid" "$id"
  sleep "${WAIT:-30}"
  xcrun simctl io "$udid" screenshot target/ios/screenshot.png
  # Still running? (A crash leaves the home screen in the screenshot.)
  xcrun simctl spawn "$udid" launchctl list | grep -i "$id" || { echo "the app isn't running" >&2; exit 1; }
  echo "screenshot $(pwd)/target/ios/screenshot.png"
}

e2e() {
  E2E=1 build "${IOS_TARGET:-aarch64-apple-ios-sim}" target/ios
  local udid=${SIM_UDID:-$(simulator "${SIM_NAME:-Pro Max}")}
  boot "$udid"
  # Store screenshots: 9:41, full bars and battery.
  xcrun simctl status_bar "$udid" override --time 9:41 --dataNetwork wifi --wifiMode active --wifiBars 3 \
    --cellularMode active --cellularBars 4 --batteryState charged --batteryLevel 100 || true
  xcrun simctl uninstall "$udid" "$id" 2>/dev/null || true
  xcrun simctl install "$udid" target/ios/Obscurify.app
  # The e2e build doesn't ask for notifications (the alert would cover the screenshots); this may allow them.
  xcrun simctl privacy "$udid" grant notifications "$id" 2>&1 || true
  python3 e2e/ios/host.py "$udid" "$id" target/ios/e2e
}

device() {
  [ "${E2E:-}" != 1 ] || { echo "no e2e driver in a device build" >&2; exit 2; }
  build aarch64-apple-ios target/ios-device
  # An .ipa is the .app in Payload/, zipped (keeps the executable bit through artifact uploads).
  (cd target/ios-device && rm -rf Payload Obscurify.ipa && mkdir Payload && cp -R Obscurify.app Payload/ && zip -qry Obscurify.ipa Payload && rm -rf Payload)
  echo "ipa $(pwd)/target/ios-device/Obscurify.ipa"
}

case "${1:-build}" in
  build) build "${IOS_TARGET:-aarch64-apple-ios-sim}" target/ios ;;
  device) device ;;
  sim) sim ;;
  e2e) e2e ;;
  listing) # App Store character limits (ios/listing/app-store.md)
    for f in name:30 subtitle:30 promotional_text:170 keywords:100 description:4000; do
      n=$(tr -d '\n' < "ios/listing/${f%%:*}.txt" | wc -m)
      echo "${f%%:*}: $n/${f##*:}"; [ "$n" -le "${f##*:}" ] || { echo "too long" >&2; exit 1; }
    done ;;
  *) echo "usage: $0 build|device|sim|e2e|listing" >&2; exit 2 ;;
esac
