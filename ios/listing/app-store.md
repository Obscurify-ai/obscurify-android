# App Store Connect answers (drafts)

The listing text is in this folder (`name.txt` 30, `subtitle.txt` 30, `promotional_text.txt` 170,
`keywords.txt` 100, `description.txt` 4000 characters; `tools/ios.sh listing` checks the limits).
It is the Play listing (`play/listing/`) said for the iPhone: the same claims, minus what the iPhone
app does differently (background answers). Screenshots: `screenshots/` (6.9", 1320x2868, from the
simulator e2e run, `tools/ios.sh e2e`). Everything here is a draft for Tony to approve.

## URLs

| Field | Value |
|---|---|
| Support URL | https://obscurify.ai/contact |
| Marketing URL | https://obscurify.ai |
| Privacy Policy URL | https://obscurify.ai/privacy |
| Terms (EULA) | Apple's standard EULA, or https://obscurify.ai/terms as a custom EULA |
| Account deletion (for review notes) | In the app: Account → Delete account. Also https://obscurify.ai/delete-account |

Category: Productivity (secondary: Utilities). Price: free. No in-app purchases.

## Age rating questionnaire

Aiming at 18+, which the Terms require. The model answers whatever it's asked within the content
guard (no sexually explicit content), so the content answers are about what a general assistant can say.

| Question | Answer |
|---|---|
| Parental controls | No |
| Age assurance | No |
| Unrestricted web access | No: links open in Safari, outside the app; there's no in-app browser |
| User-generated content (broadly distributed) | No: chats aren't shared with other users |
| Social media | No |
| Messaging and chat (between users) | No |
| Shared location | No |
| Advertising | No |
| Profanity or crude humor | Frequent |
| Horror/fear themes | Infrequent |
| Alcohol, tobacco or drug use or references | Frequent |
| Dating and relationships | No |
| Medical or treatment information | Frequent |
| Health or wellness topics | Yes |
| Mature or suggestive themes | Frequent |
| Sexual content or nudity | Infrequent |
| Graphic sexual content and nudity | No |
| Cartoon or fantasy violence | Infrequent |
| Realistic violence | Infrequent |
| Prolonged graphic or sadistic violence | No |
| Guns or other weapons | Infrequent |
| Gambling, simulated gambling, contests, loot boxes | No |

Frequent alcohol/drug references give 18+. If App Store Connect offers a higher rating than the
answers give, choose 18+.

## App Privacy ("nutrition label")

From `play/data-safety.csv` (Google Play's Data safety form) and `ios/PrivacyInfo.xcprivacy`, which
must say the same. No tracking; no data used for advertising or analytics; nothing shared with data
brokers. Data is encrypted in transit.

| Data type | Collected | Linked to the user | Tracking | Purposes | Why |
|---|---|---|---|---|---|
| Identifiers → User ID | Yes | Yes | No | App functionality | The account (an account code made on the phone; the server keeps a key derived from it) |
| Purchases → Purchase history | Yes | Yes | No | App functionality | The account's balance and plan (paid on the website, never in the app) |
| User content → Other user content | Yes | No | No | App functionality | Messages, attached files and reports, sent to answer them. Private sessions carry no account, so the server can't link a chat to one; synced chats are end-to-end encrypted |
| User content → Photos or videos | Yes | No | No | App functionality | Photos attached to a message, sent to the model and not kept |

Not collected: contact info, health, financial info, location, contacts, browsing or search
history, usage data, diagnostics, crash data, other identifiers or device IDs.

To confirm (Tony): "Other user content: not linked". Apple counts data as linked when it's tied to an
account or device; chats sent through private sessions aren't, but a signed-in Encrypted chat or a
report goes with the session. Answering "linked" is the cautious choice; "not linked" matches how
private sessions work.

## Export compliance

`ITSAppUsesNonExemptEncryption` is YES (tools/ios.sh). The app ships its own implementations of
standard algorithms, not only Apple's: TLS (rustls), AES-GCM and HMAC for encrypted chat sync, blind
RSA signatures (RFC 9474) for private sessions, and the TEE enclave's key agreement. Apple's table
(developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption):
"an industry standard algorithm, not provided within the Apple operating system" needs no CCATS, and
needs a French encryption declaration only if the app is on the App Store in France. In App Store
Connect: uses encryption, standard algorithms, then either upload the French declaration or leave
France out of the countries. The source is public (GPL), which the US rules treat as publicly
available. Decision for Tony: France or not.

## App Review notes (draft)

> Obscurify is a private AI chat app. No sign-in is needed: tap "Chat as a guest" on the first screen to
> chat with a free model. Before the first message the app asks permission to send chats to AI model
> providers (guideline 5.1.2(i)).
>
> To see the account features, create an account in the app ("Create one" on the sign-in screen):
> it takes no email or password and shows an account code. Or use this demo account code:
> [DEMO ACCOUNT CODE, with some balance, to be filled in by Tony].
>
> There are no purchases in the app. The app is a free companion to the obscurify.ai service; a
> balance and plans are managed on the website, and the app has no link or prompt to buy there.
> Without one, the free models are available.
>
> Account deletion: Account (menu ☰ → Account) → Delete account → Delete for good.
> Reporting content: every answer has a Report button (reason, optional note).
> Content: a system instruction against sexually explicit content and content that helps someone
> seriously hurt people is sent with every request, and models for sexual or role-play content
> aren't offered in this app. Users must be 18 or older (Terms; sign-up asks).
>
> Code Mode runs code the assistant writes in a separate, sandboxed WebKit view with no access to the
> app, its data or its storage; it computes results and draws charts for the answer and doesn't change
> what the app does (guidelines 2.5.2 and 4.7).

Risk to flag: App Review may question Code Mode under 2.5.2 (downloaded code). It runs only inside
WebKit (4.7 allows HTML5 and JavaScript content in a web view), but if review objects, the fallback is
to turn the Code switch off on iOS (`code::code_blocked`), as before this change.
