# Hybrid assistant — P2: dictation into another app (`insert-text`)

Speak, and the words go into the text field you were typing in, in any app,
after you confirm. Nothing is ever sent, and nothing is typed anywhere but the
field the capture started in.

## Flow

1. **Remember the field** (`assistant_voice_toggle` → `Dictation::probe`). Right
   before a capture starts, and before the island takes the keyboard for its
   card, Rust remembers the foreground window, its process and the UI
   Automation element that has the keyboard. Nothing is remembered when:
   - the foreground window is Coucou's own (the chat mic button: "Use in
     chat" covers that);
   - the focused element belongs to another process than the foreground
     window (a system popup, say).
   The field is tied to the capture id (`Dictation::bind`). A stop press keeps
   the field the capture started with. It is forgotten after 160 s (30 s of
   capture, 2 min of preview, some slack), when the next capture starts, or
   once it was used.
2. **Offer** (`assistant_insert_offer`, when the transcript arrives). Only for
   plain words: a real transcript (never the simulated one) that is not a pill
   command and not "open <app>". Rust checks the field (step 4's checks) and:
   - the field can take it: Rust builds the `insert-text` action itself, from
     its own transcript and the remembered window and process
     (`{ text, targetWindow, targetPid }`), the policy says **ask**, and the
     card gets "Insert into <app>" plus the grant. The page never names the
     target.
   - the field can't (password, read-only, no ValuePattern): the card says
     why. For a field without ValuePattern (many rich editors) it offers
     **Copy** next to "Use in chat"; never for a password field.
   - a transient `focus-moved` or `busy` refusal: **Retry**
     asks about the same remembered field. It never writes; a successful
     recheck first shows the named **Insert into <app>** confirmation.
   - no field was remembered: the card stays as before.
   `<app>` is the program's FileDescription ("Notepad", "Google Chrome"),
   or its file name.
3. **Confirm** (the click, or Enter). Rust checks the grant against its
   retained action and reserves the preview for one write (`Voice::begin_insert`,
   `policy::check_grant`). Duplicate confirmations cannot submit another write.
   The UI Automation thread checks the exact capture, window and process before
   touching the remembered field. A definitive refusal before SetValue keeps
   the original transcript, grant and preview deadline (`Voice::finish_insert`),
   without an idle event or a chat fallback. A transient `focus-moved` or `busy`
   refusal allows another explicit Insert click; other refusals leave the
   fallback actions, with no Copy for a password field. Rechecking or retrying
   never renews the original two-minute preview. Cancel, a newer capture,
   session interruption or expiry prevents a late result from restoring it.
4. **Checks**, every time (`dictation::refusal`), first failure wins:
   - `gone`: the window no longer exists, belongs to another process now
     (a reused handle), or the element no longer answers;
   - `focus-moved`: the keyboard is on another window than the target, other
     than Coucou's own (the island takes it for its card, which is expected).
     Another window of the same app counts as moved too (a dialog opened);
   - `protected`: a password field (`IsPassword`, or unreadable);
   - `read-only`: ValuePattern read-only, or the field disabled;
   - `unsupported`: no ValuePattern;
   - `busy`: the app didn't answer in time (UI Automation timeouts are
     1.5 s per call; the app waits at most 4 s per answer).
5. **Write** (`dictation/uia.rs`): `ValuePattern.SetValue` on that one
   element, never simulated keystrokes (a keystroke lands wherever the
   focus is; SetValue only on the element named). No Enter, no submit.
   - The words go where the caret was, replacing a selection, when the
     field's TextPattern reports it and the value agrees with it. Otherwise
     at the end (RichEdit can report `\r` where the value has `\r\n`; an
     offset is never guessed).
   - A space is added between words that would touch, not before
     punctuation, not after an opening bracket or quote, and never in
     Chinese or Japanese.
   - Cancellation, expiry and whether the caller is still waiting are checked
     immediately before SetValue, including after potentially slow UIA reads.
   - Afterwards the keyboard goes back to the target window.
6. **Receipt**:
   - `succeeded` (evidence `uia-value:read-back`): the value read back
     contains the words;
   - `unknown`: SetValue said yes but the value doesn't show them, or the write
     reply timed out. A timeout cannot prove that the queued/in-flight write
     did not happen. The permit is retired and the action is never retried;
   - `failed`: SetValue failed. The words then become a `chat-draft` (the
     policy allows it) and wait in Coucou's chat field, unsent, so they are
     not lost.

The log says what happened (`dictation: field remembered for the capture`,
`dictation: text inserted and read back`, `assistant action act-… succeeded`),
never the words or the app.

## Why this shape

- ValuePattern replaces the whole value, so the new value is built from the
  current one. That is why the caret must be known, and why the value must
  agree with TextPattern before an offset is used.
- UI Automation runs on one thread of its own (COM MTA, `coucou-dictation`),
  which also holds the remembered element. The rest of the app sees only
  plain data. A timed-out offer answers "busy"; a timed-out write answers
  `unknown`, since its effect cannot be ruled out.
- Windows' UIPI blocks writing to an app running as administrator from a
  normal one: such a field ends up `failed` (or `unsupported`), never
  half-written.
- Known limits: apps whose fields don't expose a writable ValuePattern
  (Word, many chat apps built on contenteditable) get the Copy fallback.
  Web pages driven by a framework (React) may show the new value but not see
  it as typed until the next keystroke.

## Verification

- Rust: `refusal` for every check, `spliced` (caret, selection, end,
  disagreeing value, spacing, accents, CJK), `verified`, the thread without a
  field; `Voice::propose` / `begin_insert` / `finish_insert` / `preview_text` /
  `fallback_draft` for `insert-text`; exact capture ownership, refusal retention,
  original expiry, duplicate confirmations, cancellation/replacement, terminal
  timeouts; the shared contract fixture (`insert-text` available, decision
  `ask`, no target or an extra field such as `pressEnter` refused). These
  regressions use simulated captures and a queued worker, without desktop access.
- Live (opt-in, `cargo test dictation -- --ignored`): a WinForms text box in a
  separate PowerShell process, with "Hello world" and the caret after
  "Hello ": the field is remembered, offered as "Windows PowerShell", written
  and read back (`dictation: text inserted and read back`). A password box is
  refused (`protected`). The tests skip themselves when Windows won't give
  the test window the keyboard, and never write anywhere but their own box.
- Windows 11 Notepad's editor (`RichEditD2DPT`) exposes a writable
  ValuePattern and a TextPattern (checked read-only).
- JS: the card offers Insert only for plain real words, writes only on the
  click, keeps the transcript and allows a retry after `focus-moved`, sends
  failed words to the chat with a receipt, never retries `unknown`, and
  ignores a late answer for a cancelled capture. Initial transient refusals
  have an explicit recheck button followed by a separate named confirmation;
  duplicate clicks, original preview expiry and password fallback rules are
  covered without microphone or desktop access.

Not verified yet: the whole flow by voice in the running app (speak into
Notepad, a browser field, a chat app).
