#!/usr/bin/env bash
# Builds lyrebird (the Tor Project's pluggable transports: obfs4, Snowflake) for Android, for the
# app's Tor bridges, into target/lyrebird/<abi>/liblyrebird.so. Android only runs programs from an
# app's native library dir, so it ships there under a library's name. Needs Go and the NDK; without
# Go the app is built without bridges (Tor still connects directly).
#
#   tools/lyrebird.sh x86_64|aarch64
set -euo pipefail
cd "$(dirname "$0")/.."
# Pinned: lyrebird 0.10.0 (gitlab.torproject.org/tpo/anti-censorship/pluggable-transports/lyrebird).
REV=59ac7e400c384773f5584dbe366516ad04a33d9f
arch=${1:?arch}
case $arch in x86_64) goarch=amd64; abi=x86_64 ;; aarch64) goarch=arm64; abi=arm64-v8a ;; *) echo "arch?" >&2; exit 1 ;; esac
out=target/lyrebird/$abi/liblyrebird.so
[ -f "$out" ] && [ "$(cat target/lyrebird/rev 2>/dev/null)" = $REV ] && exit 0
GO=(go)
if ! go version >/dev/null 2>&1 && command -v mise >/dev/null; then
  v=$(mise ls --installed go 2>/dev/null | awk '{print $2}' | sort -V | tail -1)
  [ -n "$v" ] && GO=(mise x "go@$v" -- go)
fi
"${GO[@]}" version >/dev/null 2>&1 || { echo "lyrebird: no Go, building without Tor bridges" >&2; exit 0; }
src=target/lyrebird/src
if [ "$(git -C $src rev-parse HEAD 2>/dev/null)" != $REV ]; then
  rm -rf $src && git clone -q https://gitlab.torproject.org/tpo/anti-censorship/pluggable-transports/lyrebird.git $src
  git -C $src checkout -q $REV
fi
ndk=${ANDROID_NDK_HOME:-$(ls -d "${ANDROID_HOME:-$HOME/Android/Sdk}"/ndk/* | sort -V | tail -1)}
mkdir -p "$(dirname $out)"
# -checklinkname=0: Snowflake's WebRTC (pion, via wlynxg/anet) reaches into the net package on Android.
(cd $src && CGO_ENABLED=1 GOOS=android GOARCH=$goarch \
  CC="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/$arch-linux-android26-clang" \
  "${GO[@]}" build -trimpath -ldflags="-s -w -checklinkname=0 -buildid=" -o "$OLDPWD/$out" ./cmd/lyrebird)
echo $REV > target/lyrebird/rev
