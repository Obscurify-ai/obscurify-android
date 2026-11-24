# iPhone

The iPhone app is this repo's store build, the `play` cargo feature (see `play/README.md`), compiled
for iOS. Dioxus runs the same Rust UI in WKWebView. It isn't signed for the App Store yet: CI builds
it for the simulator (where it's tested end to end) and as an ad-hoc signed `.ipa` for devices, to
re-sign for the test iPhone.

## Build

On a Mac with Xcode, Rust with `aarch64-apple-ios-sim` and `aarch64-apple-ios`, and dioxus-cli 0.7.10:

```sh
tools/ios.sh build     # target/ios/Obscurify.app for the simulator, ad-hoc signed
tools/ios.sh device    # target/ios-device/Obscurify.app and Obscurify.ipa for an iPhone, ad-hoc signed
tools/ios.sh sim       # the simulator build, launched in the newest iPhone simulator, screenshot
tools/ios.sh e2e       # the e2e build driven in an iPhone Pro Max simulator (below)
tools/ios.sh listing   # App Store character limits of ios/listing/
```

There's no Mac on our side, so `.github/workflows/ios.yml` does this on GitHub macOS runners for every
push to the `ios` branch (and by hand from the Actions tab once the workflow is on main):

- **Simulator e2e and screenshots**: lints for iOS with `-D warnings`, runs `tools/ios.sh e2e`, and
  uploads the e2e app, the screenshots and the app's own log as `obscurify-ios-simulator`.
- **Device build**: `tools/ios.sh device`, uploaded as `obscurify-ios-device` (`Obscurify.ipa`, plus
  `codesign`, `vtool` and Info.plist printouts). To put it on the test iPhone from Linux:
  `gh run download <run> -n obscurify-ios-device`, then `xtool install Obscurify.ipa` (xtool re-signs
  it with a development certificate for the phone). It hasn't run on a device yet, and Apple hasn't
  validated it: that needs the signing below.

Locally on Linux, `cargo clippy --target aarch64-apple-ios-sim --no-default-features --features play`
type-checks the iOS code if `SDKROOT` points at a directory whose `usr/include` has C headers for
aarch64 (the Android NDK's sysroot headers plus a stub `TargetConditionals.h`): ring's build script
only needs headers. It can't link.

The bundle id is `ai.obscurify.ios` (`[ios]` in `Dioxus.toml`; Android keeps `ai.obscurify.android`
and `ai.obscurify.play`). It becomes permanent when the App Store Connect record is created.

## Bundle contents

dx 0.7.10 makes a bare `.app` (binary and Info.plist). `tools/ios.sh` adds what an upload needs and
checks it on every build (the build fails without it):

| | What | From |
|---|---|---|
| App icon | `AppIcon` from one 1024x1024 PNG with no alpha; `actool` makes `Assets.car` and the home screen sizes and adds `CFBundleIcons` | `ios/AppIcon.svg` (the split "o" of the Android launcher icon, on black) rendered to `ios/Assets.xcassets/AppIcon.appiconset/AppIcon-1024.png` with `rsvg-convert` |
| Launch screen | Plain black, compiled with `ibtool` to `LaunchScreen.storyboardc` (the plist already names `LaunchScreen`) | `ios/LaunchScreen.storyboard` |
| `MinimumOSVersion` | 16.0, the `deployment_target` in Dioxus.toml; the binary is linked for the same (`IPHONEOS_DEPLOYMENT_TARGET`, checked with `vtool`). Everything used is older: WKWebView, MessageChannel in the sandbox, UserNotifications, background tasks | Dioxus.toml |
| `CFBundleVersion` | The Android `version-code` (1000860 for 0.8.60); `CFBundleShortVersionString` is the version | Cargo.toml |
| `ITSAppUsesNonExemptEncryption` | YES (below) | |
| Privacy manifest | `PrivacyInfo.xcprivacy`: no tracking, the collected data types of the App Privacy answers, and the required-reason APIs the binary imports. The build lists the binary's imports and fails on a required-reason API the manifest doesn't declare. Today only `stat`/`fstat`/`lstat` (Rust's `std::fs`): file timestamps, reason C617.1 (files in the app's own container). No `NSUserDefaults`, boot time, disk space or keyboard APIs | `ios/PrivacyInfo.xcprivacy` |
| Device family | iPhone only (`UIDeviceFamily` 1), portrait only: no iPad screenshots or iPad review | |
| Xcode keys | `DTPlatformName`, `DTSDKName`, `DTXcode`… and `CFBundleSupportedPlatforms`, as Xcode writes them | |

### Export compliance (`ITSAppUsesNonExemptEncryption`)

YES. Apple's rule: encryption limited to what the OS provides (HTTPS through URLSession) is exempt;
the app ships its own implementations of standard algorithms: TLS (rustls with ring), AES-GCM and
HMAC-SHA256 for encrypted chat sync, blind RSA signatures (RFC 9474) for private sessions, and the key
agreement and encryption to TEE enclaves in Encrypted mode. Apple's table (App Store Connect Help,
"Export compliance documentation for encryption") puts "an industry standard algorithm, not provided
within the Apple operating system" at: no CCATS, and a French encryption declaration only if the app
is on the App Store in France. Under US rules this is mass-market encryption, and the source is public
(GPL); both of those are our reading, not checked against the EAR text. In App Store Connect: the app
uses encryption, standard algorithms; then upload the French declaration or leave France out.
**Tony, 2026-11-23: file the French declaration and keep France.**

## What's different on iOS

`crate::IOS` (`cfg!(target_os = "ios")`) gates it, next to `crate::PLAY`. `src/ios.rs` refuses to
compile without `play` or with `tor`, so the iPhone app can't be built with payments, uncensored
models or Tor.

| | Android | iOS |
|---|---|---|
| Store rules | Play build: no payments, no uncensored or roleplay models, Report, account deletion, 18+ | The same build (`play`). Also: the Account screen doesn't say funds and plans are on the website (App Store 3.1.3(f) forbids even that call to buy outside the app), and the AI providers sheet below |
| AI providers sheet | None | Before the first message reaches a model, a sheet says chats go to AI model providers and asks (App Store 5.1.2(i)). "Not now" sends nothing and keeps the text; "Agree and send" is kept in `ai-consent.json`. Every completion checks it (`api::consent` in `api::chat`, `chat_encrypted` and the summaries), not just Send |
| Tor | Sideload app only | None |
| Code Mode | Sandbox WebView with no bridges (MainActivity.kt) | The same design in `ios.rs`: a WKWebView with a non-persistent data store and no app bridges, a navigation delegate that loads only `<base>/sandbox/` at the top (frames: https, data, blob, about), and one message handler, `obsSandbox`, that only passes the runner's messages to `code::on_sandbox`. The runner's MessagePort is made in its own page after it loads (runner.js takes an `init` with no source at the top level, as the Android native port arrives). The model's code runs in the runner's Worker, which can't reach the handler. It sits in the window at 1 pt, all but invisible, so iOS doesn't pause it. Pages open full screen in a WKWebView made the same way with no handler, under a bar with the title and Close. A sandbox whose web process dies fails the run at once (`fatal`) |
| Links | `android.open` → an intent | `webbrowser::open` → Safari |
| Copy | `ClipboardManager` | `UIPasteboard` (the page's clipboard API would need a user gesture) |
| Long answers | Foreground service with a notification, wake lock, "answer ready" notification | The screen stays on while an answer is worked on. Leaving the app, `beginBackgroundTask` gives the answer iOS's usual half minute or so, and if it ends with the app in the background a local notification says "Your answer is ready" (permission asked when the first answer starts, as on Android). Nothing is drawn while the app is in the background: iOS suspends the WebView, and Dioxus polls no task while a render waits for it. A signed-in answer the app couldn't finish is finished by the server and picked up on return. A Code Mode turn's steps and its sandbox stall in the background too (both are web views), and carry on when the app is back. `backgrounded()` is also true while the app is merely inactive (a system alert in front, such as the notification prompt that comes with the first answer), so that answer draws only once the alert is answered: check on a device |
| Backups | Android's default (allowBackup not set, so Google backup may include the files) | Application Support goes into iCloud and computer backups except the session, the keys for private chats and chat sync, and the chats (`store::not_backed_up`: `session.json`, `unlinkable-seed.json`, `unlinkable-state.json`, `sync-seed.json`, `chats.json`, `pending.json`, every `chat-*.json`), marked `isExcludedFromBackup` after every save and at launch. Signing in again brings the keys back, and synced chats come back from sync. Settings and `wallet.json` (blind vouchers, which the store build can't buy) are backed up. Chats out too: Tony, 2026-11-23 |
| Back | System Back asks the page (`obsBack`) | No Back button: screens have their own ← |
| Network back | `ConnectivityManager` callback → `obsOnline` | The page's `online` event |
| Files | `/data/user/<id>/<package>/files` | `$HOME/Library/Application Support` in the app container |
| Attach | File input | The same file input; WKWebView shows the photo library, camera and Files. `NSCameraUsageDescription` and `NSPhotoLibraryUsageDescription` are in `[ios.plist]` |
| User agent | `Obscurify-Android` | `Obscurify-iOS` (the server counts sign-ups from `Obscurify-Android*` as the app; iOS sign-ups aren't counted yet) |
| Reports | "from the Android app (Google Play, …)" | "from the iPhone app (App Store, …)" |

## e2e

`tools/ios.sh e2e` builds with the `e2e` cargo feature, which runs `e2e/ios/driver.js` inside the
app's own page (it clicks and types as a person would and reports on the app's stderr), and
`e2e/ios/host.py` launches it in an iPhone Pro Max simulator, follows that log and does what the page
can't: screenshots (with the status bar at 9:41), sending the app to the background and back, and
reading the backup flag on the app's files. It runs against production as a guest. With
`E2E_SIGNUP=1` (CI only when the workflow is run by hand, never on a push) it also makes a production
account, checks the account screens and deletes the account from the app, also when the run fails
after making it. After the e2e, CI launches the real store build (no e2e driver) in the same
simulator and checks it keeps running; only the e2e build is driven. No secrets, and no screenshot of an account code: artifacts of this public repo are
public. The e2e build never ships: `tools/ios.sh device` refuses `E2E=1` and fails if the driver's
marker is in the binary. The e2e build doesn't ask for notifications (the alert would cover the
screenshots), so the permission request has never run anywhere yet; the log says whether the "answer
ready" notification was handed to iOS and the authorization status (not asked, in CI), so it isn't
shown there.

## Parity with the Play build

### On the test iPhone 13 (iOS 26.1), 2026-11-23

Device build from run 45135457044, re-signed with xtool (free team, so the bundle id on the phone is
`XTL-97QU7SP64C.ai.obscurify.ios`) and driven over WebDriverAgent:

- Works: launch and icon, guest chat streaming a real answer, the consent sheet before the first
  send, the notification permission prompt after the first answer, Copy ("Answer copied"),
  a markdown link opening Safari, and an answer finishing while another app was in front.
- Not confirmed: the "answer ready" notification on screen (Notification Center can't be opened
  from WebDriverAgent, no banner caught).
- Found: bare URLs in answers are plain text, not tappable (only markdown links are links).
  When the keyboard opens, the whole page scrolls up and the top bar (☰, model picker, ＋)
  goes under the status bar until the keyboard closes.
- Fixed in 44720ea (run 45194169440, checked on the phone the same day): a bare address in an answer
  is a link and opens Safari; with the keyboard open the top bar stays put and the message box
  sits just above the keyboard.


Every user-facing feature of the Play build (play/README.md, the Android UI and e2e/*.mjs) and where
it stands on the iPhone. "Simulator" means `e2e/ios/driver.js` checked it in CI (run linked);
"same code" means the iPhone runs the same Rust and page as Android but no iOS test has driven it;
"device" means it depends on native behaviour the simulator run can't show (pickers, the keyboard,
Safari, notifications on screen).

Runs: https://github.com/Obscurify-ai/obscurify-android/actions/runs/45132255600 (everything, with a new account made and deleted; the screenshots in `ios/listing/screenshots/` are from it) and https://github.com/Obscurify-ai/obscurify-android/actions/runs/45135457044 (the latest push: guest flows, plus the real store build launched and kept running).

| Feature (Play build) | iPhone | How we know |
|---|---|---|
| Chat as a guest (a free model, 30 messages an hour) | Works | Simulator: chat as a guest, streamed answer with markdown and folded thinking |
| AI providers sheet before the first message (iOS only) | Works | Simulator: opens on Send, nothing sent before agreeing, "Not now" keeps the text, "Agree and send" sends, asked once |
| Create an account (account code made on the phone, 18+, no referral code) | Works | Simulator: sign-up screen (18+, no referral field), account made, chat signed in |
| Sign in with an account code or a 16-digit number; typo check | Same code | Not driven on iOS (Android: account-code.mjs) |
| Old 16-digit account → account code upgrade sheet | Same code | Not driven on iOS |
| Private sessions (chats with no account attached, wallet from the account code) | Same code | Keys made and saved at sign-up (backup log); chats in the run were as a guest |
| Model picker: search, categories, providers, favorites (drag to order), unavailable models greyed | Same code | Simulator: picker opens with its categories (screenshot). Drag and search not driven |
| No uncensored or roleplay models in the picker | Works | Simulator: no Uncensored or Roleplay category |
| Content guard first in every completion (`api::play_guard`) | Same code | Checked on Android through a recording proxy (play.mjs); no proxy in the iOS run |
| Tools: web search, paid tools billed to the balance | Same code | The Tools switch is on in the run; no paid tool called (needs a funded account) |
| Code Mode: Python and JavaScript in a sandbox, charts | Works | Simulator: Python ran in the sandbox WKWebView, its chart shown, sum 385 |
| Code Mode: pages full screen with Close | Works | Simulator: page opened full screen (screenshot); Close button itself not tapped (native, e2e closes it from Rust) |
| Code Mode sandbox sealed off (no app bridges, no navigation away, no storage) | Built | Design as on Android (ios.rs); the Android checks from inside the sandbox (code.mjs) aren't ported |
| Attach photos and files (PDF, Word, Excel, PowerPoint, CSV, text) | Device | The file input and reading are the same; the iOS picker (photos, camera, Files) needs a device |
| Encrypted mode (TEE, checked on the phone) | Same code | Not driven on iOS (Android: encrypted.mjs) |
| Chat sync, end to end encrypted, "Combine chats?" | Same code | The Sync switch is on after sign-up (Account screenshot); syncing not driven |
| Chat history drawer, Temp chats, Wipe | Same code | Not driven on iOS |
| Copy | Device | `UIPasteboard` (ios.rs); needs a device to see |
| Raw, Edit, Retry, Stop, Continue | Same code | Not driven on iOS |
| Report on every answer | Works | Simulator: Report opens the form with reasons (not sent) |
| Account: balance, plan, account code, API keys; no payments, no plan buying, no word about buying elsewhere | Works | Simulator: Account screen, no Add funds and no buy-elsewhere text |
| Delete account in the app | Works | Simulator: the new account deleted from Account, back at sign-in |
| Sign out | Same code | Not driven on iOS |
| Long answers with the app in the background, "answer ready" notification | Partly | Simulator: with Settings in front, an answer kept streaming for 11 s and finished in the background, and the notification request was handed to iOS (`addNotificationRequest` succeeded; not authorized, so not shown). iOS gives about 30 s, not Android's foreground service; past that the server finishes a signed-in answer and the app picks it up (same code, not driven). The notification on screen and the permission prompt need a device |
| Lost connection: waits and carries on | Same code | iOS uses the page's `online` event; not driven |
| Answer picked up after the app was killed (server-held) | Same code | Not driven on iOS (Android: background.mjs) |
| Compaction of long chats, agent runs (100 steps) | Same code | Not driven on iOS |
| Mirrors when the server is unreachable | Same code | Not driven on iOS |
| Links open in the browser | Device | `webbrowser` → Safari; needs a device |
| Keyboard, safe areas, screen staying on | Device | Simulator screenshots show the safe areas right; the keyboard and idle timer need a device |
| Back | Different | No system Back on iOS; every screen has its own ← |
| Tor, payments, vouchers, plans, referral, uncensored | Left out | As in the Play build (`play`), and `ios.rs` won't compile with Tor |

## Store listing

Drafts in `ios/listing/`: name, subtitle, promotional text, keywords and description (`.txt`, limits
checked by `tools/ios.sh listing`), and `app-store.md` with the URLs, age rating answers, App Privacy
answers (from `play/data-safety.csv`, matching `PrivacyInfo.xcprivacy`), export compliance and App
Review notes. Screenshots: `ios/listing/screenshots/` (6.9", 1320x2868, from the e2e run). All
waiting for Tony's approval.

## Still to do

- **TestFlight** (below): signing is set up; the App Store Connect app record, a validate run and the
  first upload are next.
- **On the test iPhone**: install `obscurify-ios-device` with xtool and check what the simulator can't
  show: the attach picker, Copy, links to Safari, the keyboard, the notification prompt and the "answer
  ready" notification on screen, a long answer past iOS's background time.
- **More of the Android e2e on iOS**: sign in with a code and a number, sync, Encrypted, edit/retry/stop,
  paid tools. Paid tools and Encrypted need a funded account as a CI secret: Tony's call.
- **Server**: sign-ups from `Obscurify-iOS` aren't counted as the app yet.
- **Release**: `tools/release.mjs` doesn't build or upload the iPhone app; a version bump doesn't either.
- **App Review risks**: Code Mode under 2.5.2 (fallback: Code off on iOS through `code::code_blocked`);
  models that need a plan bought on the website ("needs the Pro plan") under 3.1.1/3.1.3(f).

### Decided (Tony, 2026-11-23)

1. Bundle id `ai.obscurify.ios`.
2. Chats stay out of backups too (above).
3. The AI providers sheet wording stays as it is.
4. Age rating: not 18+ unless Apple's answers make it so. The answers in `ios/listing/app-store.md`
   give 16+; the Terms and the sign-up screen still say 18+.
5. Export compliance: file the French declaration and keep France.
6. Code Mode stays on iOS for the first submission (fallback if App Review objects under 2.5.2:
   Code off through `code::code_blocked`).

Still his to approve: the listing text, the App Privacy answers and review notes in `ios/listing/`,
and a funded demo account for review. Done here and kept unless he says otherwise: the Account
screen on iOS doesn't say funds and plans are on the website (3.1.3(f)); iPhone only and portrait
only; the e2e runs against production (as a guest on every push; a by-hand run makes and deletes an
account).

## Signing and TestFlight

Set up 2026-11-24, all from Linux through the App Store Connect API (`tools/asc.py`, a JWT from the
API key): team **CryptoTalks, LLC**, team id `TZGL5RGW6K` (not the free team `97QU7SP64C` xtool uses).

| What | Where |
|---|---|
| App ID `ai.obscurify.ios` | bundle id resource `8NNN97R59R`, no capabilities |
| API key "obscurify-ios-ci-2", App Manager | key id and issuer in repo secrets `ASC_KEY_ID`, `ASC_ISSUER_ID`; the `.p8` in `ASC_KEY_P8` and `~/.config/apple-developer/` (Apple lets you download it once) |
| Apple Distribution certificate `N7SW9WK7UH`, to 2027-11-24 | `.p12` in `IOS_DIST_P12` (base64) and `IOS_DIST_P12_PASSWORD`; key, CSR and `.p12` in `~/.config/apple-developer/signing/`. The `.p12` uses the legacy algorithms (`openssl pkcs12 -legacy -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1`), which `security import` reads |
| App Store profile "Obscurify App Store" `6LB2PQB626`, to 2027-11-24 | `IOS_PROFILE` (base64) |

- **CI** (`ios.yml`, device job): with the secrets present, every push signs the device build for
  the App Store (`tools/ios.sh store`: a temporary keychain, the profile embedded, its entitlements
  with the keychain group narrowed to the app, `codesign --generate-entitlement-der`). A manual run
  with `store: validate` also checks the `.ipa` with Apple (`altool --validate-app`), and
  `store: upload` sends it to TestFlight. The signed `Obscurify-store.ipa` stays out of the artifacts.
- **Version.** `CFBundleShortVersionString` is `version` in `Cargo.toml`, `CFBundleVersion` the
  Android `version-code`: every upload needs a higher one.
- **Still to do:** the App Store Connect app record (the API can't create apps; the web form asks
  for the company name shown on the App Store, which can't be changed: Tony's call), then a validate
  run, then upload and internal TestFlight on the test iPhone. Apple also wants the EU trader status
  (Digital Services Act) before EU distribution, and a support URL on a site that names the
  company (Apple support, case 103023671995: obscurify.ai doesn't mention CryptoTalks, LLC).
- **Renewals.** The certificate and profile expire 2027-11-24, the same day the membership renews
  ($98.99/yr, App Store subscription on the Apple Account, cancel in Settings → Apple Account).
- The test iPhone can also run a development-signed build (a development certificate and a profile
  listing its UDID) without TestFlight.

The release path stays Android-only for now: `tools/release.mjs` doesn't know about iOS, and a
version bump doesn't build the iPhone app.
