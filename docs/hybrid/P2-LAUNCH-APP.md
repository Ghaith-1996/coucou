# Hybrid assistant — P2: "open <app>" (`launch-app`)

The Mac's "ouvre Figma" (`VoiceQuery.appToOpen` → `NSWorkspace`), on Windows,
from a controlled list and only after the user confirms.

## Flow

1. **Parse** (`src/assistant/intents.ts`, `appToOpen`): a port of the Mac's.
   The words after the first open verb ("ouvre", "ouvrir", "open"), past "moi",
   articles and "app". This is checked before the pill commands, as on the
   Mac. One change: more than four words after the verb is dictation, not a
   name ("…please open the chat and show me the latest pull request").
2. **Find** (`assistant/apps.rs`): the controlled list is **Windows' own "All
   apps" list** (the shell's AppsFolder, what Start and `Get-StartApps` show),
   Store apps included. On this PC it has 286 items, 238 after filtering. Start
   menu shortcuts alone would miss Calculator, Notepad, Settings and the Store
   Spotify.
   - Left out: uninstallers, readmes, help, licences, web links and documents.
   - Matching, first match wins: the exact name, then the name's first words
     ("Visual Studio" → Visual Studio Code), then one whole word ("word" →
     Microsoft Word, not WordPad), then Whisper's near spellings (Levenshtein
     ≤ 2, names of 4+ letters only). Shortest name on a tie.
   - French names of Windows' own apps ("calculatrice", "bloc-notes",
     "paramètres"…), only when the spoken name itself finds nothing.
3. **Propose** (`Voice::propose`): the policy must say **ask**.
   - Rust keeps the `launch-app` action (`{ appId }`, an id made from the
     entry, never a path), which expires with the transcript (2 min).
   - It returns the app's name *as Start shows it* and a **grant** bound to
     the action id, the expiry and the normalised arguments (SHA-256,
     `policy::binding_digest`).
   - The card shows "Open <that name>".
4. **Confirm** (`Voice::confirm`, on the user's click or Enter): the grant
   comes back and Rust checks it against the action *it* kept, for this
   capture, still in time.
   - Everything is decided again (`check_grant`).
   - The proposal and the transcript are used once, whatever the answer. A
     changed digest, a later expiry or another action id gives
     `grant-mismatch`.
5. **Open** (`assistant/launch.rs`): the app is found again by its id in a
   fresh list, then opened as Start opens it: `ShellExecuteEx` on
   `shell:AppsFolder\<AppID>`, with no arguments and no error dialog.
6. **Receipt**: `succeeded` needs a visible, unowned top-level window belonging
   to the process returned by `ShellExecuteEx`. Its handle stays open during
   the wait so its PID cannot be reused. A foreground change alone is not
   evidence: the user may have switched to an unrelated app. Store apps and
   apps already running can open through another process; without a window
   correlated to the returned process within 10 s, the outcome is `unknown`,
   never retried automatically. An app
   that disappeared from the list in the meantime gives `failed`
   (`unavailable`). The log has the action id and outcome, never the app's
   name.

Nothing opens without the click. A simulated transcript never looks for an
app. "Use in chat" stays available next to "Open <app>".

## Initial implementation verification

These are the original checks. The stricter evidence check below replaces the
foreground-only success rule used by the original Calculator smoke test.

| Check | Result |
|---|---|
| Contract fixture (Rust + TS) | `launch-app` available; a valid one → **ask**; a path/command as id → invalid-args; any extra field → invalid-args |
| Rust unit tests | matching (exact before longer, words, Whisper spellings, short names), French names (an app really called that wins), ids (policy-safe, distinct per entry, case-insensitive), filtering (uninstallers, links, documents, quotes/control characters), proposal → confirm once, forged digest / later expiry / other action → mismatch and the proposal is gone, cancel drops it, only `{appId}` and only for a waiting transcript |
| JS tests | the Mac's 4 VoiceQueryTests cases + Whisper's punctuation, politeness, articles, dictation not taken for a name; the card finds then offers the app, opens nothing before the click, sends back exactly Rust's grant, a missing app says so and keeps "Use in chat", a late answer for an older capture changes nothing, a simulated transcript never looks |
| Real list (opt-in) | 238 apps; Calculator, Notepad, Spotify (Store), VS Code, Paint, Discord found; "calculatrice" → Calculator |
| Real launch (opt-in) | Calculator opened in 0.3 s and was closed afterwards, but the original foreground-only receipt did not prove correlation. Not rerun with the stricter evidence rule; Store/reused-process activation may now report `unknown`. An AppID not in the list → `failed`, no dialog |
| Totals | `scripts\cargo-vs.cmd test --workspace --locked` 484 + 30 pass (7 opt-in ignored); `npm test` 644 pass |
| **Not verified** | The whole path spoken through the live microphone in the running app (each step is covered separately) |

### Evidence-correlation regression checks

`tests/assistant-launch-evidence.test.mjs` guards the native source against
foreground-only success, requires the shell PID evidence path, and checks that
the process handle remains owned through polling. These three source-contract
checks failed before the fix and pass after it. `assistant/launch.rs` also has
unit cases for a matching visible window, an unrelated PID, missing launch PID
(Store/reused-process activation), and hidden/owned windows. The Rust cases and
opt-in real-app smoke tests require Windows and were not run in the Linux review
environment, which has no Rust toolchain; no application was launched.
