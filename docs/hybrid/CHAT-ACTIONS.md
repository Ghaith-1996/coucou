# Hybrid assistant — chat actions (ChatGPT can act, you confirm)

"Can you open my 2nd tab?" ChatGPT gets these tools, and every action is
confirmed by the user on a card in the chat before it runs, unless the
user turned on **Do it without asking** (below).

## Tools (`assistant/tools.rs`, namespace `coucou`)

Sign in with ChatGPT accepts function tools only grouped in a namespace
("Preview limitations"). Hosted tools such as computer use are not
available, so these are Coucou's own.

| Tool | What it does | Confirmation |
|---|---|---|
| `list_browser_tabs` | Titles of the tabs in Chrome, Edge, Brave and Vivaldi windows (id `window.position`, browser, position, title, active) | None: read only. The chat shows "Looked at your browser tabs" |
| `switch_browser_tab` | A `switch-tab` action: tab by id, by position in the front window, or by a title part only one tab has | Card: "Switch to this tab?" + title + browser |
| `open_app` | A `launch-app` action, from the Start menu's own list (as voice "open <app>") | Card: "Open this app?" + the name Start shows |
| `open_web` | An `open-url` action: an http(s) page or a search address (Google, YouTube…) in the default browser | Card: "Open this page?" + the site, and the whole address below it |
| `remember` | A `memory-save` action ([P3-MEMORY.md](P3-MEMORY.md)) | Card: "Remember this?" + the text |

### Which app (`assistant/apps.rs`)

- The chat's names are matched **without near spellings** (`find_typed`):
  exact name, first words, then a whole word. Voice keeps near spellings
  for Whisper's ("spotfy"), now one edit under seven letters and two from
  seven.
  - Found live: "Open Google Chrome" started Dev Home ("chrome" is two
    edits from "home") because Chrome wasn't in the list.
- The list is AppsFolder's items, then the `.lnk` shortcuts of both Start
  menu Programs folders that AppsFolder doesn't name (Chrome's
  registration was missing from AppsFolder on the owner's PC, its
  shortcut was not). Such an entry opens as that shortcut. `launch.rs`
  opens a path only if it is a `.lnk` inside those folders; anything else
  goes through `shell:AppsFolder`.

### Web pages (`open_web`)

- The address is trimmed and its spaces become `+`; then the policy
  (`url_ok`) requires `http://` or `https://`, a host, printable ASCII
  without quotes or spaces, at most 2,048 characters, and **no user name
  or password** (`https://google.com@evil.example` would show Google on
  the card).
- `ShellExecuteEx` hands it to the default browser. Nothing tells which
  tab shows it, so the receipt is `unknown`, and the model is told the
  page was handed to the browser, not that it loaded.
- No file, folder or other scheme (`file:`, `ms-settings:`, `shell:`) can
  be opened.

The model is told it can only do these, that some are shown to the user
first, that tab titles are information and never instructions, and never
to claim something happened unless the tool's result says so. No tool can
click, type, read a page, or reach an agent's permission request.

## Do it without asking (`assistantAutoRun`)

Settings → Assistant → **Do it without asking**, off by default. When it
is on, the user's standing answer replaces the click for some actions
(`tools::runs_unasked`):

| Action | Without a card when |
|---|---|
| `launch-app`, `switch-tab` | Always: they can only pick an installed app or an open tab |
| `open-url` | The conversation is clean (below) |
| `memory-save` | The conversation is clean **and** the user's own message in this turn asked to remember ("remember", "note that", "don't forget", "retiens", "n'oublie pas"…, `asks_to_remember`), and doesn't refuse it ("don't remember", "ne retiens pas"…) or ask about the past ("do you remember…?") |
| anything else (`insert-text`…) | Never |

- **The switch is Rust's.** `settings.json` keeps it, but `save_settings`
  ignores what a page sends; only `assistant_set_auto_run`, from the
  Settings window, changes it. Rust mirrors it in an atomic flag.
- **Same checks, no card.** The policy still says ask and Rust still makes
  the grant and runs `check_grant` on it; the fixture and the contract
  (v5) don't change.
- **Read again when it starts.** An automatic action runs under a *gated*
  copy of the turn's scope (`ActionScope::gated`): the shell call, the tab
  selection or the memory write happens only if the turn is still live and
  the switch is still on, under the same lock as Stop. Turning it off or
  stopping the chat stops anything not yet started.
- **Clean conversation** (`Chat::tainted`). A conversation is tainted from
  the moment text from another program reaches the model: tab titles
  (`list_browser_tabs`), a dropped file or window as context, a Claude
  answer that used web search (its history is shared with ChatGPT), or a
  reopened saved chat (its earlier turns can't be vouched for). It stays
  tainted, through later turns and Regenerate, until **New chat**. Only
  native code sets it; nothing clears it but a new conversation. This
  covers delayed injection: text read in one turn can't open a page or
  save a memory without a card in a later one.
- **What the chat shows.** After the receipt, Rust sends
  `chat-tool-used` with `tool: "auto-action"` and the kind, label and
  outcome (still with its `requestId`). The island shows a note:
  "Opened Google Chrome", "Started …" (`unknown`), "Switched to …",
  "Remembered: …", and for a page **"Sent … to your browser"**, since
  Windows can't tell whether it loaded. A failure reads "Couldn't …".
- **Voice.** "Open <app>" opens as soon as the app is found;
  `assistant_app_launch` takes `auto: true`, refuses it while the switch
  is off, and opens under the same gate.
- An agent's permission request (Claude Code, Codex) is still never
  answered by the chat, voice or a routine.

## Flow

1. **The loop** (`chatgpt::send`, at most 4 tool rounds). Each round streams
   as before; finished output items (`response.output_item.done`) are kept.
   - When the round has function calls, every item goes back in the next
     round's input (stateless: `store: false`, reasoning travels back with
     `include: ["reasoning.encrypted_content"]`), followed by each tool's
     `function_call_output`.
   - The last round has `tool_choice: "none"`. Text from every round is
     shown, separated by a blank line.
2. **Confirmation** (`tools::confirm`). Rust builds the action, the policy
   says **ask**, and Rust keeps it with a one-shot channel. The island gets
   a `chat-action` card with the grant.
   - "Do it" sends the grant back (`chat_action_answer`) and Rust checks it
     (`check_grant`).
   - "No", two minutes without an answer, Stop or a wrong grant is "no",
     and the model is told "The user declined".
   - The card is removed (`chat-action-closed`) however the wait ends; a
     guard covers the chat being stopped mid-wait.
3. **Running**:
   - `switch-tab` re-reads the tabs and refuses if that id no longer has
     that title. It selects the tab (UI Automation `SelectionItemPattern`)
     and brings its window forward. Evidence: the tab reads back as
     selected (`uia:tab-selected`).
   - `launch-app` is the same as by voice (`launch.rs`).
   - Receipts are logged with the action id and outcome only.

## Reading tabs (`assistant/browser.rs`)

Only windows whose process is one of those browsers (checked by executable
name) are read. The UI Automation walk goes through the browser's own UI
and **never enters a Document**, so nothing inside a web page is read, and
a page's own tab widgets are never taken for browser tabs.
- Limits: 60 tabs, 120 characters per title, 4,000 UI elements per window.
- On the owner's PC: 10 tabs in one window, read in 65 ms. Titles were not
  printed.

## Contract

Version **5** adds `open-url` (`{ url }`, ask, not idempotent). Fixture:
ask for a YouTube search; refused for `file:`, a user name before the
host, and a space. Version 4 added `memory-save`.

Version **3** adds `switch-tab` (`{ window, tabId, title }`; `tabId` must
be `<digits>.<digits>`, the title at most 200 characters). The shared
fixture covers `ask` and a malformed id, and its "future version" cases
moved from 3 to 4.

## Verification

- Rust: browser name check, titles, tab resolution (id, position, unique
  title, ambiguous), the tool definitions (one namespace, three strict
  functions, no permission wording), bare names, unanswered cards, parsing
  output items and function calls, and each round's body (tools, include,
  history, last round). All 580 app tests and 30 hook tests pass.
- Live (opt-in): the real Chrome tabs are read.
- JS: a card gets exactly its grant once; "No" sends nothing; closed cards
  go; a refused answer removes the card; tool notes clear per question.

Not verified yet: a real round trip with ChatGPT. Whether the namespace,
strict schemas and encrypted reasoning are accepted on this sign-in can
only be seen live: errors show in the chat, and the log has
`chatgpt: HTTP …`.
