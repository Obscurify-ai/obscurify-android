#!/usr/bin/env bash
# Builds the Obscurify APK: dx makes the Android project, then this copies in MainActivity.kt and the
# black theme/icon, sets the versionCode, builds with Gradle and zipaligns.
#
#   ./build.sh            arm64 phone  -> target/obscurify.apk
#   ./build.sh x86_64     emulator
#
# Signing: if a release keystore exists (OBSCURIFY_KEYSTORE, default ~/.config/obscurify-android/
# release.keystore, password in OBSCURIFY_KEYSTORE_PASS or keystore-pass next to it) the APK is signed
# with it, so it installs over earlier releases. Without one you get target/obscurify-unsigned.apk.
# Test builds only: OBSCURIFY_API=https://... points the app at another server; DEVTOOLS=1 lets the
# e2e scripts drive the WebView. Never ship either.
set -euo pipefail
cd "$(dirname "$0")"
if [ -z "${JAVA_HOME:-}" ] && command -v mise >/dev/null; then JAVA_HOME=$(mise where java@temurin-21 2>/dev/null || true); fi
[ -n "${JAVA_HOME:-}" ] && export JAVA_HOME PATH="$JAVA_HOME/bin:$PATH"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
BT=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
ks=${OBSCURIFY_KEYSTORE:-$HOME/.config/obscurify-android/release.keystore}
arch=${1:-aarch64}

# versionCode comes from Cargo.toml ([package.metadata.android] version-code), so every build of a
# release has the same one and F-Droid can read it.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
code=$(sed -n 's/^version-code = \([0-9]*\)/\1/p' Cargo.toml | head -1)

dx build --platform android --release --target $arch-linux-android
gen=target/dx/obscurify/release/android/app
sed -i "s/versionCode = [0-9]*$/versionCode = $code/" $gen/app/build.gradle.kts
man=$gen/app/src/main/AndroidManifest.xml
grep -q ACCESS_NETWORK_STATE $man || sed -i 's#<uses-permission android:name="android.permission.INTERNET" />#&\n    <uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />#' $man
cp MainActivity.kt $gen/app/src/main/kotlin/dev/dioxus/main/MainActivity.kt
[ "${DEVTOOLS:-}" = 1 ] && sed -i 's#web = webView$#web = webView; WebView.setWebContentsDebuggingEnabled(true)#' $gen/app/src/main/kotlin/dev/dioxus/main/MainActivity.kt
cp -r android-res/. $gen/app/src/main/res/ # icon, black theme
# dx leaves the other architecture's library behind; ship only this one.
abi=$([ $arch = x86_64 ] && echo x86_64 || echo arm64-v8a)
find $gen/app/src/main/jniLibs -mindepth 1 -maxdepth 1 ! -name $abi -exec rm -rf {} +
(cd $gen && ./gradlew --quiet assembleRelease)
out=$gen/app/build/outputs/apk/release/app-release-unsigned.apk
rm -f target/obscurify.apk target/obscurify-unsigned.apk
if [ -f "$ks" ]; then
  pass=${OBSCURIFY_KEYSTORE_PASS:-$(cat "$(dirname "$ks")/keystore-pass")}
  "$BT/zipalign" -f 4 $out target/obscurify-aligned.apk
  "$BT/apksigner" sign --ks "$ks" --ks-pass "pass:$pass" --out target/obscurify.apk target/obscurify-aligned.apk
  echo "built $(pwd)/target/obscurify.apk ($version, $code)"
else
  "$BT/zipalign" -f 4 $out target/obscurify-unsigned.apk
  echo "built $(pwd)/target/obscurify-unsigned.apk ($version, $code); no keystore at $ks, so not signed"
fi
