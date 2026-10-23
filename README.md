# Obscurify for Android

Private AI chat from [Obscurify](https://obscurify.ai): hundreds of models behind one anonymous account.
There's no email, username or password, only a 16-digit account number. You pay with vouchers the
server signs without seeing them, so a payment can't be linked to the account it funds.

The app is a small, native Rust client of the Obscurify API (Dioxus 0.7 in the system WebView). Chats
are stored only on the phone.

## Features

- **Account number sign-in.**
  - Sign-up shows the number once.
  - The app keeps only the session, never the number.
  - You can replace the number from the Account page.
- **Chat.**
  - Answers stream in as they're written.
  - Web search, image attachments (shrunk on the phone before upload) and collapsible reasoning.
  - Copy, Retry, Edit and Stop.
  - Markdown is rendered safely: raw HTML shows as text, links open in your browser, and remote images become links, so they never load by themselves and reveal your IP.
- **Models.** Search, categories (Free, Cheap, Reasoning, Uncensored, Roleplay, Frontier) and favorites.
- **Account.** Balance, plans (paid from the balance), API key, referral link and transactions.
- **Add funds with unlinkable vouchers.**
  - Pay by card, Bitcoin, Lightning or Monero.
  - The app blinds random voucher messages (RFC 9474, RSABSSA-SHA384-PSS-Deterministic). The server signs them blind, and the app unblinds them into voucher codes.
  - Each code is added to the balance at its own random time within the hour, so timing doesn't link the payment to the account either.
  - Orders go out with no cookie or key.
  - Every step is saved before the next request, so a crash never loses a payment.

## Build

Requirements:

- Rust with the `aarch64-linux-android` target (`x86_64-linux-android` for the emulator)
- The Android SDK with the NDK and build tools
- Java 17 or newer
- `dioxus-cli`: `cargo install dioxus-cli --version 0.7.10 --locked`

```sh
./build.sh          # arm64 phone: target/obscurify.apk, or obscurify-unsigned.apk without a keystore
./build.sh x86_64   # emulator
```

The versionCode is `version-code` in `Cargo.toml` (1000000 + major·10000 + minor·100 + patch); bump it with the version.

Releases are signed with the Obscurify release key, so updates install over each other. The key isn't
in this repo. A build without it is unsigned, and you can sign it with your own key.

Its certificate's SHA-256 is
`7c:a4:01:a8:fe:d2:19:c1:7f:19:7a:f6:b0:7c:10:d0:63:09:7f:b6:39:98:90:b5:8b:58:d5:8c:d8:72:a7:1c`.

Every release carries the APK twice: `obscurify-<version>.apk`, and `obscurify.apk`, which
obscurify.ai/android links through `releases/latest/download/obscurify.apk`. Upload both.

## Code

| File | What |
|---|---|
| `src/main.rs` | App state, the JS bridge (links, Back, image picker, pinned scrolling), sign-in, sign-up, Account, Add funds |
| `src/chat.rs` | Chat, streaming, chats on the phone, model picker, safe markdown |
| `src/api.rs` | HTTP. Signed-in calls carry the session; voucher calls carry nothing |
| `src/wallet.rs` | Voucher orders: blind, sign, unblind, redeem |
| `src/store.rs` | JSON files in the app's private storage |
| `MainActivity.kt` | The only Kotlin: Back asks the page first; opening links and wallet URIs; copy |
| `assets/style.css` | OLED black, green trim |

## Tests

The tests are end to end only: they drive the real app on an emulator against an Obscurify dev server
with the regtest payment stack.

```sh
OBSCURIFY_API=https://<dev server> DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
cd e2e && npm i playwright
BTC_RPC_PASSWORD=… node app.mjs   # sign-up, BTC voucher top-up, chat, web search, models, restart, new number, sign in
node card.mjs                     # image picker and a vision model; Stripe test-card top-up
```

## License

GPL-3.0. See [LICENSE](LICENSE). The Obscurify service the app talks to is not open source.
