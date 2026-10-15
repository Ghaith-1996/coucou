# Hybrid assistant — chat actions (ChatGPT can act, you confirm)

"Can you open my 2nd tab?" ChatGPT gets these tools, and every action is
confirmed by the user on a card in the chat before it runs.

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

The model is told it can only do these, that each is confirmed, and never
to claim something happened unless the tool's result says so. No tool can
click, type, read a page, or reach an agent's permission request.

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
