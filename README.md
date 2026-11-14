# Obscurify for Android

Private AI chat from [Obscurify](https://obscurify.ai): hundreds of models behind one anonymous account.
There's no email, username or password, only a 16-digit account number. A payment goes straight on
the account's balance, or, if you choose, buys blind vouchers the server signs without seeing them, so
the payment can't be linked to the account it funds.

The app is a small, native Rust client of the Obscurify API (Dioxus 0.7 in the system WebView). Chats
are stored only on the phone.

## Features

- **Tools and Code.** Two switches in the message line, both on unless you turn them off. Tools: the assistant searches the web and finds and uses paid tools billed to the balance, calling them directly. Code: it runs code on the phone and makes charts and pages. With both on it calls tools directly and writes code only when the job needs it; with both off it's plain chat. Guests get them too, except paid tools.
- **Chat as a guest**, with no account.
- **A lost connection doesn't fail anything.** Tool calls and answers wait for up to 15 minutes and deliver when it's back. Paid calls carry a request id, so the server runs them once.
- **Long-running agent.** A reply can take 100 steps and a run 30 minutes, and keeps going in the background with a notification. What it has spent shows as it goes. A lost connection is waited out for as long as it takes. A request that keeps failing goes back to the model. Requests only ever append, which keeps the provider's prompt cache working, and a conversation near the model's context is compacted into a summary instead of having old results trimmed.
- **Nothing paid is thrown away.** A failed step retries by itself, then the answer stops, keeps its runs and tool results, and offers Continue; a new message carries them too, and they survive the app closing.
- **Answers keep going when the app closes.** A signed-in answer is written on the server whatever happens to the phone, and held there for up to an hour. A dropped connection picks it up from where it was, and one that died quietly while the phone slept is replaced the moment the app is back on screen or online; an app that was closed or killed finishes it into its chat when it opens again, and Code Mode carries on its run. Stop ends it on the server too.
- **Encrypted.** End-to-end encrypted chat with TEE models (confidential-computing enclaves), using [obscurify-tee-verify](https://github.com/Obscurify-ai/tee-verify). Before anything is sent, the phone checks the enclave gateway's attestation for a nonce of its own (a genuine Intel TDX machine running an approved gateway release) and encrypts every message to the key that attestation vouches for. Obscurify's server passes the request to the enclave without being able to read it, and each piece of the answer comes back encrypted to the phone's key. Then the receipt is checked: the reply's bytes are the ones the gateway signed, the enclave received exactly the conversation the phone encrypted, and the gateway verified the model host. Each answer gets a chip with the checks: "Encrypted" when every one passes, "Check failed" when one doesn't. If the attestation doesn't check out, the phone says "Not encrypted" and sends nothing. Chats from the old Verify mode keep their "Verified" or "Not verified" chips, since they weren't encrypted. While Encrypted is on, the picker lists only the models that take encrypted requests, and tools, web search and summarizing long chats are off (they would need the text).
- **Temp, Encrypted, Tor and Wipe** at the top of the chat list. Temp starts a chat that is never written to the phone. Tor opens the onion site. Wipe deletes every chat and setting on the phone but keeps the sign-in and the wallet, since unspent vouchers are money.
- **Account number sign-in.**
  - Sign-up shows the number once.
  - The app keeps only the session, never the number.
  - You can replace the number from the Account page.
- **Chat.**
  - Answers stream in as they're written.
  - Web search, image attachments (shrunk on the phone before upload) and collapsible reasoning.
  - One attach button for photos and files. PDFs, spreadsheets (CSV, TSV, Excel, OpenDocument), Word, PowerPoint and text or code files are read on the phone and sent as text, so every model can use them. The file itself never leaves the phone.
  - Copy, Retry, Edit and Stop.
  - Markdown is rendered safely: raw HTML shows as text, links open in your browser, and remote images become links, so they never load by themselves and reveal your IP.
- **Code Mode.** The assistant can run Python (numpy, pandas, matplotlib) and JavaScript on the phone to calculate, analyze data and draw charts; build pages and small apps that open full screen; search the web; and use paid tools billed to your balance (at most $1 per reply).
  - Code runs in a WebView of its own with none of the app's bridges. It loads only the server's `/sandbox/` pages, whose CSP gives them an opaque origin and no network, and every other request is refused at the native layer. It talks to the app over one native message port.
  - Code, data and results stay on the phone; only tool calls leave it.
- **Models.** Search, categories (Free, Cheap, Reasoning, Uncensored, Roleplay, Frontier) and favorites. Models and categories your plan includes come first; the rest are listed last.
- **Account.** Balance, plans (paid from the balance), API key, referral link and transactions.
- **Add funds.** Pay by card, Bitcoin, Lightning or Monero.
  - By default it's a top-up tied to the account: the server puts every confirmed payment on the balance by itself, so you can close the app, and any device lists it under Payments.
  - **Blind voucher** (off each time Add funds opens) makes the payment unlinkable. The app blinds random voucher messages (RFC 9474, RSABSSA-SHA384-PSS-Deterministic), the server signs them blind, and the app unblinds them into voucher codes. Each code is added to the balance at its own random time within the hour, so timing doesn't link the payment to the account either. Orders go out with no cookie or key.
  - The catch: until a blind voucher is added it exists only on this phone, and the app adds it only while it's open. Open the app again after paying, and don't uninstall it or clear its data before the credit is on the balance, or the money is lost. Copy codes keeps them safe. The app warns and asks before every blind payment.
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

Every release carries exactly one APK, `obscurify.apk` (the tag holds the version), which
obscurify.ai/android links through `releases/latest/download/obscurify.apk`. Never add a second
APK: Obtainium then asks which one to install.

## Code

| File | What |
|---|---|
| `src/main.rs` | App state, the JS bridge (links, Back, image picker, pinned scrolling), sign-in, sign-up, Account, Add funds and top-ups |
| `src/chat.rs` | Chat, streaming, chats on the phone, model picker, safe markdown |
| `src/files.rs` | Reading attached files into text: PDF, spreadsheets, Word, PowerPoint, OpenDocument, text |
| `src/code.rs` | Code Mode: the agent loop, the sandbox bridge, tools the model calls directly, budgets, runs on the message |
| `src/tee.rs` | Encrypted: checking TEE replies against their receipts and attestation, the chip (sealing a request is `api::chat_encrypted`) |
| `src/compact.rs` | Compaction: earlier messages summarized in chunks so a chat goes on past the model's context |
| `src/api.rs` | HTTP. Signed-in calls (top-ups too) carry the session; blind voucher calls carry nothing |
| `src/wallet.rs` | Top-ups (`/top_ups`), and blind voucher orders: blind, sign, unblind, redeem |
| `src/store.rs` | JSON files in the app's private storage |
| `MainActivity.kt` | The only Kotlin: Back asks the page first; opening links and wallet URIs; copy; Code Mode's sandbox WebView and full-screen pages |
| `assets/style.css` | OLED black, green trim |

## Tests

The tests are end to end only: they drive the real app on an emulator against an Obscurify dev server
with the regtest payment stack.

```sh
OBSCURIFY_API=https://<dev server> DEVTOOLS=1 ./build.sh x86_64 && adb install -r target/obscurify.apk
cd e2e && npm i playwright
BTC_RPC_PASSWORD=… node app.mjs   # sign-up, BTC blind voucher, chat, web search, models, restart, new number, sign in
BTC_RPC_PASSWORD=… node top-ups.mjs  # BTC top-up paid while the app is closed, added on relaunch; the Blind voucher warning and confirm
node card.mjs                     # image picker and a vision model; Stripe test-card blind voucher
ACCOUNT=… PLAN=casual node plans.mjs  # models the plan can use first, the rest under "Upgrade to use"
ACCOUNT=… node agent.mjs             # Long runs, compaction, errors back to the model, background work (scripted replies, through flaky-proxy.mjs)
ACCOUNT=… node code.mjs              # Code Mode: Python, a chart, a paid tool, a page full screen, the sandbox sealed off
node flaky-proxy.mjs & ACCOUNT=… node switches.mjs  # Tools and Code switches: on by default, a lookup is a direct web_search, code off, tools off (code can't reach them), both off is plain chat
ACCOUNT=… node parity.mjs            # guest chat and tools, Tools switch, provider filter, raw, edit, plan confirmation, delete account
node flaky-proxy.mjs & ACCOUNT=… node outage.mjs  # lost connections wait and deliver; PAID=1 also loses a paid answer and checks it's charged once
node flaky-proxy.mjs & ACCOUNT=… node resume.mjs  # failures keep tool work: retries, Continue, new messages, Retry only on failures, a killed app; the Copy toolbar (test APK with OBSCURIFY_API=http://127.0.0.1:8093, adb reverse tcp:8093 tcp:8093)
ACCOUNT=… node files.mjs             # files: PDF, CSV, Excel, Word, PowerPoint, OpenDocument, text; refusals; a long file cut
node categories.mjs                  # specialist categories (Writing, Security, Translation…) from the server's model tags
node flaky-proxy.mjs & ACCOUNT=… node background.mjs  # background answers: a dropped connection, a killed app (plain and Code Mode), Stop
node flaky-proxy.mjs & adb root; ACCOUNT=… node encrypted.mjs   # Encrypted (no request ever asks for tee_verify): e2ee models only, a marker word never leaves the phone, Encrypted chip after eight checks, history recalled, a swapped key sends nothing, a changed byte fails, a killed app reads the rest, kept after a restart
TARGET=<dev server> node flaky-proxy.mjs & ACCOUNT=<Pro> node refusal.mjs  # Claude refusals (server's fake upstream): "declined" in italics, no button the first time, then "Ask <model> instead" bottom right that switches and asks again, plain and Code Mode; "Uncensored models need Pro" opens Account; the suggestion isn't kept
```

## License

GPL-3.0. See [LICENSE](LICENSE). The Obscurify service the app talks to is not open source.
