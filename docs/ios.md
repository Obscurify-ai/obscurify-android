# iPhone

The iPhone app is this repo's store build, the `play` cargo feature (see `play/README.md`), compiled
for iOS. Dioxus runs the same Rust UI in WKWebView. It is not signed yet, so for now it runs only in
the iOS Simulator.

## Build

On a Mac with Xcode, Rust with `aarch64-apple-ios-sim` (and `aarch64-apple-ios` for devices) and
dioxus-cli 0.7.10:

```sh
tools/ios.sh build   # target/ios/Obscurify.app for the simulator, ad-hoc signed
tools/ios.sh sim     # the same, then boots the newest iPhone simulator, installs, launches, screenshots
```

There's no Mac on our side, so `.github/workflows/ios.yml` does this on a GitHub macOS runner for
every push to the `ios` branch (and by hand from the Actions tab once the workflow is on main). It
lints for iOS with `-D warnings`, runs `tools/ios.sh sim`, and uploads the `.app`, the screenshot,
the app's log, any crash reports and a listing of the app's files as the `obscurify-ios-simulator`
artifact.

The bundle id is `ai.obscurify.ios` (`[ios]` in `Dioxus.toml`; Android keeps `ai.obscurify.android`
and `ai.obscurify.play`). It becomes permanent when the App Store Connect record is created.

## What's different on iOS

`crate::IOS` (`cfg!(target_os = "ios")`) gates it, next to `crate::PLAY`. `src/ios.rs` refuses to
compile without `play` or with `tor`, so the iPhone app can't be built with payments, uncensored
models or Tor.

| | Android | iOS |
|---|---|---|
| Store rules | Play build: no payments, no uncensored or roleplay models, Report, account deletion, 18+ | The same build (`play`). App Store review asks for the same things |
| Tor | Sideload app only | None |
| Code Mode | Sandbox WebView with no bridges (MainActivity.kt) | **Off.** `code::code_blocked()` is true, as with Tor: no Code switch, the model never gets `run_code`, pages don't open. Tools (web search, paid tools) still work |
| Links | `android.open` → an intent | `webbrowser::open` → Safari (`ios.rs`) |
| Copy | `ClipboardManager` | `UIPasteboard` (`ios.rs`); the page's clipboard API would need a user gesture |
| Long answers | Foreground service with a notification, wake lock, "answer ready" notification | The screen stays on while an answer is worked on (`isIdleTimerDisabled`). Nothing keeps the app running in the background. The server holds a signed-in answer for an hour, so an app that iOS suspended picks it up when it's back |
| Back | System Back asks the page (`obsBack`) | No Back button: screens have their own ← |
| Network back | `ConnectivityManager` callback → `obsOnline` | The page's `online` event |
| Files | `/data/user/<id>/<package>/files` | `$HOME/Library/Application Support` in the app container |
| Attach | File input | The same file input; WKWebView shows the photo library, camera and Files. `NSCameraUsageDescription` and `NSPhotoLibraryUsageDescription` are in `[ios.plist]` |
| User agent | `Obscurify-Android` | `Obscurify-iOS` (the server counts sign-ups from `Obscurify-Android*` as the app; iOS sign-ups aren't counted yet) |
| Reports | "from the Android app (Google Play, …)" | "from the iPhone app (App Store, …)" |

What CI shows is that the app builds, launches and draws its first screen in the simulator. Copy,
links, the picker, safe areas, the keyboard and what happens when iOS suspends the app need a real
iPhone, so a signed build.

## Still to do

- **Code Mode on iOS.** A second WKWebView made in Rust (objc2-web-kit is already in the tree):
  no script message handlers, a non-persistent data store, `WKNavigationDelegate` allowing only
  `/sandbox/` (and https subresources, as on Android), talking to the app through
  `evaluateJavaScript` and one message handler on that WebView alone. Then full-screen pages the same way.
- **Background.** `beginBackgroundTask` for the last seconds of an answer, and a local notification
  when it's ready (`UNUserNotificationCenter`).
- **Backups.** Application Support goes into iCloud and device backups. The session and the
  private-session wallet probably shouldn't: mark those files `isExcludedFromBackup`, or decide
  that it's fine.
- **Store listing:** screenshots, privacy "nutrition label" (the Play `data-safety.csv` answers),
  age rating 18+, the account deletion and report flows pointed out to review.

## Signing and TestFlight (once the Apple developer account exists)

Nothing below is set up yet. All of it runs on the macOS runner, so it needs secrets in the repo
(Settings → Secrets → Actions), never files in this public repo.

1. **App ID.** Register `ai.obscurify.ios` in the developer portal (Certificates, Identifiers &
   Profiles), no special capabilities.
2. **App Store Connect API key.** Users and Access → Integrations → App Store Connect API, role App
   Manager. Keep the `.p8` file, its key id and the issuer id as secrets
   (`ASC_KEY_P8`, `ASC_KEY_ID`, `ASC_ISSUER_ID`). The key lets CI create certificates and profiles
   and upload builds, with no Apple Account password or 2FA on the runner.
3. **Distribution certificate.** An "Apple Distribution" certificate, exported as a `.p12` with a
   password (`IOS_DIST_P12` base64, `IOS_DIST_P12_PASSWORD`). In CI: make a temporary keychain,
   import the `.p12`, allow codesign to use it.
4. **Provisioning profile.** An App Store profile for `ai.obscurify.ios` and that certificate
   (`IOS_PROFILE` base64), installed into `~/Library/MobileDevice/Provisioning Profiles/`.
   fastlane `match` or `xcodebuild -allowProvisioningUpdates` with the API key can replace steps 3
   and 4.
5. **Device build.** `dx bundle --platform ios --release --target aarch64-apple-ios
   --no-default-features --features play --package-types ipa`, then sign with the certificate,
   the profile embedded as `embedded.mobileprovision` and an entitlements file
   (`application-identifier`, `com.apple.developer.team-identifier`). Check with
   `codesign -dv --verbose=4`.
6. **Version.** `CFBundleShortVersionString` comes from `version` in `Cargo.toml`; every upload needs
   a higher `CFBundleVersion` than the last one (the Android `version-code` works).
7. **Upload.** `xcrun altool --upload-app -f Obscurify.ipa -t ios --apiKey … --apiIssuer …` (or
   fastlane `pilot`). The build shows up in TestFlight
   after Apple processes it; internal testers can install it at once, external testers after a
   short beta review.
8. **The test iPhone** can also run a development-signed build (a development certificate and a
   profile listing its UDID) without TestFlight.

The release path stays Android-only until then: `tools/release.mjs` doesn't know about iOS, and a
version bump doesn't build the iPhone app.
