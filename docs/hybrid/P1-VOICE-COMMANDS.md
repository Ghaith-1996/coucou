# Hybrid assistant — P1: spoken pill commands (Mac parity)

The Mac's voice intents (`NotchBuddy/Sources/App/Voice/VoiceIntent.swift`) that
are actually implemented: music, pills, state questions, mail, open an app, web
search. For Windows:

| Mac intent | Windows now | Why |
|---|---|---|
| Pills: add, remove, main, replace, keep only (and several in one sentence) | **Ported** | Internal to Coucou and reversible, same rules as Settings → Active pills |
| Music (Apple Music / Spotify) | Recognised, not offered | No Windows media adapter yet (audit: after the core) |
| Open an app | Not ported | P2 `launch-app`, which needs confirmation |
| Mail, web search, state questions | Not ported | Out of the MVP |

## How it works

1. `src/assistant/intents.ts` is a line-by-line port of `IntentParser.swift` and
   `EntityResolver.swift`: French and English, deterministic, aliases, then exact
   name, word match and Levenshtein ≤ 2, plus multi-action splitting. Music is
   parsed too, so ambiguous verbs resolve as on the Mac ("lance Spotify" is music,
   not a pill). Whisper's trailing punctuation is stripped.
2. `src/assistant/pill-command.ts` turns the intents into a **plan**: the whole
   resulting main pill and active pills, plus a label ("Add GitHub · Remove
   Vercel"), or a reason it can't be done ("Already 4 pills: remove one first.").
   The Mac acts at once and asks a follow-up when the island is full. Here the card
   shows the change and one click or Enter does it.
3. Doing it is a new contract kind **`pill-set`** (`{ mainPill, activeIntegrations }`,
   available, idempotent). Rust checks its closed schema (pill-id format, at most 4,
   no duplicates, main not repeated), and the policy allows it, since it's exactly
   what Settings does. `Voice::act` accepts only `pill-set` from the page; a chat
   draft is still built from Rust's own copy of the transcript. Either way the
   transcript is used once.
4. The island applies it as Settings would (`loadIntegrationTasks`, save) and
   reports a receipt with evidence only when it shows exactly the target.

A **simulated** transcript is never taken for a command.

This slice advances the shared assistant contract to **v2** because `pill-set`
adds to the closed v1 action registry. Rust emits v2 voice events, and the
island uses the same version for its synthesized listening state. Both parsers
reject other event versions; known actions with another version get the policy's
`version` error before expiry or argument checks. The shared fixture
keeps v1 and future-version rejection cases alongside the v2 cases. This is
version tagging, not an IPC handshake: `assistant_voice_act` is new in this
slice, and an older backend does not have that command. Unknown action kinds
still fail deserialization before they can reach the policy.

## Verification

| Check | Result |
|---|---|
| Mac table `tests/IntentParserTests.swift` | converted case for case to `fixtures/mac-intent-cases.json`: **165/165 pass** on the port |
| `npm test` | **593 pass** (+8 Windows parser/plan tests, +3 island flow tests) |
| `scripts\cargo-vs.cmd test --workspace` | **445 pass** (+4 pill-set fixture cases, +2 `act` tests) |
| Real Whisper → parser | 4 spoken commands (Windows offline TTS) → Whisper → `parseTranscript` → plan: "Add GitHub, then remove **Versal**" → add GitHub + remove **Vercel** (fuzzy match); "Switch to Cursor" → main pill; "Replace **Versal** with Notion" → replace Vercel; "Keep only GitHub and Stripe" → keep only |
| Live app | boots and the island renders with the new modules (smoke screenshot) |
| **Not verified** | A spoken command through the live microphone, then the button, then the pills changing in the running island. Each step is covered separately; no live capture was recorded while the owner was using the PC. |

Whisper's vocabulary prompt now includes the service pill names too. The TTS voice
still says "Versal", which is what the fuzzy match is for.
