#!/usr/bin/env bash
# Builds the Obscurify APK: dx makes the Android project, then this copies in MainActivity.kt and the
# black theme/icon, sets the versionCode, builds with Gradle and zipaligns.
#
#   ./build.sh                 arm64 phone  -> target/obscurify.apk (the sideload / GitHub release app)
#   ./build.sh x86_64          emulator
#   ./build.sh play            the Google Play bundle, arm64 + x86_64 -> target/obscurify-play.aab
#   ./build.sh play x86_64     the Play app as an emulator APK -> target/obscurify-play.apk
#
# The Play build (cargo feature `play`) is its own app, ai.obscurify.play: no payments in the app and
# no uncensored or roleplay models. It targets API 36 with 16 KB-aligned native code, as Play requires.
# Google signs what it ships; we sign the bundle with the Play upload key (OBSCURIFY_UPLOAD_KEYSTORE,
# default ~/.config/obscurify-android/play-upload.keystore, alias upload, password in play-upload-pass
# next to it). The sideload release key never goes to Google.
#
# Signing: if a release keystore exists (OBSCURIFY_KEYSTORE, default ~/.config/obscurify-android/
# release.keystore, password in OBSCURIFY_KEYSTORE_PASS or keystore-pass next to it) the APK is signed
# with it, so it installs over earlier releases. Without one you get target/obscurify-unsigned.apk.
# The sideload app has in-app Tor (src/tor.rs) with bridges: tools/lyrebird.sh builds the bridge transport
# (needs Go). The Play app is built without Tor.
# Test builds only: OBSCURIFY_API=https://... points the app at another server; DEVTOOLS=1 lets the
# e2e scripts drive the WebView; APP_ID/APP_LABEL install it beside the real app. Never ship any.
set -euo pipefail
cd "$(dirname "$0")"
if [ -z "${JAVA_HOME:-}" ] && command -v mise >/dev/null; then JAVA_HOME=$(mise where java@temurin-21 2>/dev/null || true); fi
[ -n "${JAVA_HOME:-}" ] && export JAVA_HOME PATH="$JAVA_HOME/bin:$PATH"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
ks=${OBSCURIFY_KEYSTORE:-$HOME/.config/obscurify-android/release.keystore}
play=
if [ "${1:-}" = play ]; then play=1; shift; fi
arch=${1:-}

# versionCode comes from Cargo.toml ([package.metadata.android] version-code), so every build of a
# release has the same one and F-Droid can read it.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
code=$(sed -n 's/^version-code = \([0-9]*\)/\1/p' Cargo.toml | head -1)
# A version bump on main is released automatically (GitHub + Play), and the release refuses one
# without its store changelog and Signals Tony. Fail here first, before anything is pushed.
changelog=fastlane/metadata/android/en-US/changelogs/$code.txt
if [ ! -s "$changelog" ]; then
  echo "no changelog for $version: write $changelog (what changed, for the Play store) before building" >&2
  exit 1
fi

gen=target/dx/obscurify/release/android/app
# The two apps share dx's generated project: start it afresh when switching, so neither keeps the
# other's package id, SDK level or libraries.
flavour=${play:+play}; flavour=${flavour:-sideload}
[ "$(cat target/.flavour 2>/dev/null)" = "$flavour" ] || rm -rf $gen
abi() { [ $1 = x86_64 ] && echo x86_64 || echo arm64-v8a; }
# A bundle carries every architecture: dx builds one at a time, so each library is kept aside.
archs=${arch:-aarch64}; [ -n "$play" ] && [ -z "$arch" ] && archs="x86_64 aarch64"
libs=$(mktemp -d); trap 'rm -rf $libs' EXIT
for a in $archs; do
  dx build --platform android --release --target $a-linux-android ${play:+--no-default-features --features play}
  mkdir -p $libs/$(abi $a) && cp $gen/app/src/main/jniLibs/$(abi $a)/libmain.so $libs/$(abi $a)/
  # Tor bridges (src/tor.rs): lyrebird beside libmain.so. Not in the Play app, which has no Tor.
  if [ -z "$play" ]; then
    tools/lyrebird.sh $a
    [ -f target/lyrebird/$(abi $a)/liblyrebird.so ] && cp target/lyrebird/$(abi $a)/liblyrebird.so $libs/$(abi $a)/
  fi
done
mkdir -p target && echo $flavour > target/.flavour
rm -rf $gen/app/src/main/jniLibs && cp -r $libs $gen/app/src/main/jniLibs
sed -i "s/versionCode = [0-9]*$/versionCode = $code/" $gen/app/build.gradle.kts
# lyrebird runs as a program, so native libraries are unpacked on install (they stay in the APK otherwise).
if [ -z "$play" ] && ! grep -q useLegacyPackaging $gen/app/build.gradle.kts; then
  printf '\nandroid { packaging { jniLibs { useLegacyPackaging = true } } }\n' >> $gen/app/build.gradle.kts
fi
if [ -n "$play" ]; then
  # Play only takes native code that works with 16 KB memory pages (Android 15+).
  for so in $libs/*/libmain.so; do
    "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf -lW $so | awk '$1 == "LOAD" && $NF != "0x4000" { bad = 1 } END { exit bad }' \
      || { echo "$so isn't aligned for 16 KB pages" >&2; exit 1; }
  done
  # Android 16 (API 36): Google Play's minimum target for new apps and updates.
  sed -i -e 's/compileSdk = [0-9]*$/compileSdk = 36/' -e 's/targetSdk = [0-9]*$/targetSdk = 36/' $gen/app/build.gradle.kts
  grep -q suppressUnsupportedCompileSdk $gen/gradle.properties || echo android.suppressUnsupportedCompileSdk=36 >> $gen/gradle.properties
  sed -i 's/applicationId = "ai.obscurify.android"/applicationId = "ai.obscurify.play"/' $gen/app/build.gradle.kts
  grep -q 'applicationId = "ai.obscurify.play"' $gen/app/build.gradle.kts || { echo "couldn't set the Play package id" >&2; exit 1; }
fi
# Test builds only: APP_ID=... installs beside the real app (a preview on a phone that has it), with
# APP_LABEL as its name under the icon.
if [ -n "${APP_ID:-}" ]; then
  sed -i "s/applicationId = \"ai\.obscurify\.[a-z]*\"/applicationId = \"$APP_ID\"/" $gen/app/build.gradle.kts
  grep -q "applicationId = \"$APP_ID\"" $gen/app/build.gradle.kts || { echo "couldn't set the package id" >&2; exit 1; }
  [ -n "${APP_LABEL:-}" ] && sed -i "s#android:label=\"[^\"]*\"#android:label=\"$APP_LABEL\"#" $gen/app/src/main/AndroidManifest.xml
fi
man=$gen/app/src/main/AndroidManifest.xml
grep -q ACCESS_NETWORK_STATE $man || sed -i 's#<uses-permission android:name="android.permission.INTERNET" />#&\n    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />#' $man
# A long answer runs in a foreground service, with a notification. Declared to Google under App content.
if ! grep -q WorkService $man; then
  sed -i 's#<uses-permission android:name="android.permission.INTERNET" />#&\n    <uses-permission android:name="android.permission.FOREGROUND_SERVICE" />\n    <uses-permission android:name="android.permission.FOREGROUND_SERVICE_DATA_SYNC" />\n    <uses-permission android:name="android.permission.POST_NOTIFICATIONS" />\n    <uses-permission android:name="android.permission.WAKE_LOCK" />#' $man
  sed -i 's#</application>#    <service android:name="dev.dioxus.main.WorkService" android:exported="false" android:foregroundServiceType="dataSync" />\n    &#' $man
fi
cp MainActivity.kt $gen/app/src/main/kotlin/dev/dioxus/main/MainActivity.kt
[ "${DEVTOOLS:-}" = 1 ] && sed -i 's#web = webView$#web = webView; WebView.setWebContentsDebuggingEnabled(true)#' $gen/app/src/main/kotlin/dev/dioxus/main/MainActivity.kt
cp -r android-res/. $gen/app/src/main/res/ # icon, black theme
# Build tools may only exist once Gradle has installed them (as on F-Droid's build server).
BT=$(ls -d "$ANDROID_HOME"/build-tools/* 2>/dev/null | sort -V | tail -1 || true)

if [ -n "$play" ]; then
  uks=${OBSCURIFY_UPLOAD_KEYSTORE:-$HOME/.config/obscurify-android/play-upload.keystore}
  upass=${OBSCURIFY_UPLOAD_KEYSTORE_PASS:-$(cat "$(dirname "$uks")/play-upload-pass" 2>/dev/null || true)}
  [ -f "$uks" ] || { echo "no Play upload key at $uks" >&2; exit 1; }
  rm -f target/obscurify-play.aab target/obscurify-play.apk
  if [ -z "$arch" ]; then
    (cd $gen && ./gradlew --quiet bundleRelease)
    cp $gen/app/build/outputs/bundle/release/app-release.aab target/obscurify-play.aab
    jarsigner -keystore "$uks" -storepass "$upass" -sigalg SHA256withRSA -digestalg SHA-256 target/obscurify-play.aab upload >/dev/null
    echo "built $(pwd)/target/obscurify-play.aab ($version, $code)"
  else
    (cd $gen && ./gradlew --quiet assembleRelease)
    "$BT/zipalign" -f 4 $gen/app/build/outputs/apk/release/app-release-unsigned.apk target/obscurify-play-aligned.apk
    "$BT/apksigner" sign --ks "$uks" --ks-pass "pass:$upass" --out target/obscurify-play.apk target/obscurify-play-aligned.apk
    echo "built $(pwd)/target/obscurify-play.apk ($version, $code)"
  fi
  exit 0
fi

(cd $gen && ./gradlew --quiet assembleRelease)
out=$gen/app/build/outputs/apk/release/app-release-unsigned.apk
rm -f target/obscurify.apk target/obscurify-unsigned.apk
if [ -f "$ks" ]; then
  pass=${OBSCURIFY_KEYSTORE_PASS:-$(cat "$(dirname "$ks")/keystore-pass")}
  "$BT/zipalign" -f 4 $out target/obscurify-aligned.apk
  "$BT/apksigner" sign --ks "$ks" --ks-pass "pass:$pass" --out target/obscurify.apk target/obscurify-aligned.apk
  echo "built $(pwd)/target/obscurify.apk ($version, $code)"
elif [ -n "$BT" ]; then
  "$BT/zipalign" -f 4 $out target/obscurify-unsigned.apk
  echo "built $(pwd)/target/obscurify-unsigned.apk ($version, $code); no keystore at $ks, so not signed"
else
  cp $out target/obscurify-unsigned.apk
  echo "built $(pwd)/target/obscurify-unsigned.apk ($version, $code); unsigned, not zipaligned (no build tools found)"
fi
