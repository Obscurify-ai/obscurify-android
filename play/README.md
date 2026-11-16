# Google Play

The Play app is `ai.obscurify.play`, the `play` cargo feature of this repo (off by default, so the
sideload and F-Droid builds leave it out):

```sh
./build.sh play          # target/obscurify-play.aab, signed with the upload key, for Play
./build.sh play x86_64   # the same app as an emulator APK (e2e/play.mjs)
```

What it changes, and the Play rule behind each change. It fails closed: the Funds screen never
renders, the model list is filtered where it loads, and every completion goes through `api::play_guard`.

| Change | Rule |
|---|---|
| No payments in the app (card, Bitcoin, Lightning, Monero, vouchers, top-ups), no plan switching, no referral program, no links to website pages that sell plans. The Account page says, as plain text, that funds and plans are handled on obscurify.ai | [Payments](https://support.google.com/googleplay/android-developer/answer/9858738): digital goods bought in the app must use Play Billing. A consumption-only app may say where to buy, without a link ([FAQ](https://support.google.com/googleplay/android-developer/answer/10281818)) |
| No uncensored or roleplay models, even through search or a suggestion after a refusal (`chat::offered`); a short system instruction against sexual content and serious harm on every completion | [AI-generated content](https://support.google.com/googleplay/android-developer/answer/13985936), [sexual content](https://support.google.com/googleplay/android-developer/answer/9878810) |
| Report on every answer | AI-generated content: users must be able to flag output without leaving the app |
| Delete account in the app (both builds) and at https://obscurify.ai/delete-account | [Account deletion](https://support.google.com/googleplay/android-developer/answer/13327111) |
| Target API 36, edge to edge, 16 KB-aligned native code (build.sh checks it), an app bundle | [Target API](https://support.google.com/googleplay/android-developer/answer/11926878), [16 KB pages](https://developer.android.com/guide/practices/page-sizes) |
| "I'm 18 or older" at sign-up (both builds) | Matches the Terms; content rating |

A new feature that takes money, sells a plan, or reaches uncensored models must be gated with
`crate::PLAY` in the same change; `e2e/play.mjs` checks the build through flaky-proxy.mjs (no payment
requests, no left-out models, the guard first in every new chat).

## Signing

Play App Signing with a key Google generates; we only hold the **upload key**
(`~/.config/obscurify-android/play-upload.keystore` on the release machine). It is not the sideload
release key, which never goes to Google. Upload key certificate SHA-256:
`11:87:18:2F:16:C6:2E:EF:A5:9F:A5:95:01:C3:B1:FC:90:75:A0:A6:4C:7E:F2:B6:0B:2B:DD:27:E5:3F:F0:65`

## Store listing

`listing/` (title, short and full description, icon, feature graphic, 1080x1920 phone screenshots;
retake them with `PKG=ai.obscurify.play ACCOUNT=… node e2e/play-screenshots.mjs`, never showing
uncensored models, prices to buy, or crypto). `data-safety.csv` is the Data safety form, entered
with Play Console's CSV import. A feature that collects something new needs that form updated by hand
before its release reaches review.
