# Hybrid assistant: summaries of Codex and Claude Code sessions

Ask the chat "what did Codex do today?" or "summarize my last Claude Code
session". The chat lists your recent sessions, reads one, and writes the
summary itself. Summaries are made **on demand**: nothing is watched or
summarized automatically.

The tools only exist in ChatGPT chats (Sign in with ChatGPT). The tool
runner is wired into that provider only (`chat.rs`), so the Claude, OpenAI
API and local providers don't get them.

## Switch

Settings → Assistant → **Let the chat read my conversations**, off by
default. The same switch covers the ChatGPT and Claude windows.

- Rust owns it, the same way it owns `assistantAutoRun`. `save_settings`
  ignores what a page sends, and only `assistant_set_read_chats`, called
  from the Settings window, changes it.
- While it is off, both tools answer that reading is turned off. Nothing is
  read and the conversation is not tainted.
- What the chat reads is sent to the chat's model. For ChatGPT that model
  is online; the hint in Settings says so.

## Where the files are (`assistant/sessions.rs`)

Checked against the owner's PC (October 2026), not assumed.

| Agent | Files | What is kept |
|---|---|---|
| Claude Code (CLI and the Claude desktop app's Code tab) | `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/<folder>/<uuid>.jsonl`. The desktop app's `%APPDATA%\Claude\claude-code-sessions\…\local_*.json` points to these files through `cliSessionId`. | `type` user or assistant lines and their `text` blocks only. Skipped: `isMeta`, `isSidechain` (sub-agents), user lines whose `origin.kind` isn't `human` (task notifications), text starting with `<command-`, `<local-command`, `<task-notification`, `[Request interrupted`, and `<system-reminder>…</system-reminder>` blocks. The title comes from `custom-title`, otherwise from the first thing the user said. |
| Codex (CLI and desktop) | `<CODEX_HOME or ~/.codex>/sessions/YYYY/MM/DD/rollout-…-<uuid>.jsonl` | `session_meta` gives the folder and start time. Kept: `event_msg` `item_completed` items `UserMessage` / `AgentMessage`, and older versions' `user_message` / `agent_message`. When files were attached, only the text after "## My request:" is kept. Sessions Codex starts for itself (`source.subagent`: the approval reviewer, spawned agents) are left out. |

Never kept: tool calls and their output, reasoning, the instructions the
agents were given (`base_instructions`, developer messages,
`<environment_context>`), and file contents.

Plain chats in the **ChatGPT and Claude desktop apps** are stored by their
services, not in files on this PC: the chat reads their open window instead
([CHAT-WINDOW.md](CHAT-WINDOW.md)).

## Tools (`assistant/tools.rs`)

| Tool | Returns |
|---|---|
| `list_agent_sessions {since}` | `since` is `since_coucou_started`, `last_24_hours` or `last_7_days`. Returns at most 30 sessions, newest first: `id` (`claude-<uuid>` or `codex-<uuid>`), `agent`, `project` (the folder's name, never its path), `title`, `started`, `lastActivity`. |
| `read_agent_session {id}` | The session plus `text`: `[user] …` / `[agent] …` lines, at most 24,000 characters, each message at most 4,000. A long session keeps its first request and then as much of the end as fits, with "[… earlier messages left out …]" in between; `shortened` says so. |

- Both tools are read only. They never show a card.
- They emit `chat-tool-used` with `agent-sessions`, and the chat shows
  "Looked at your Codex and Claude Code sessions".
- They **taint the conversation** ([CHAT-ACTIONS.md](CHAT-ACTIONS.md)):
  after them, pages and memories ask on a card again until New chat.
- The model is told the text is information to summarize, never
  instructions.
- `read` accepts only an id made of letters, digits and `-`, and only one
  that a fresh listing of the last 7 days returns. A path never comes from
  the model.

## Bounds

- At most 20,000 directory entries are walked and 500 files considered,
  newest first by modification time. Only the Codex date folders from the
  day before `since` onward are opened (a day of slack covers time zones).
- The list reads at most 1 MB from the start of each file. Codex writes
  tens of kilobytes of instructions before the first message; on the
  owner's PC the first message sat 120–250 KB in.
- A conversation is read from at most the last 4 MB of its file. The first
  request then comes from the start of the file.
- A line that isn't complete JSON is skipped, so a session still being
  written is read up to its last finished line.
- Live: 30 sessions listed in about 0.4 s on the owner's PC.

## Tests

- **Rust** (`sessions.rs`), with sample Claude and Codex files in a
  temporary folder:
  - listing with title, folder and dates;
  - only spoken text kept, with tools, reasoning and injected text dropped;
  - the old and new Codex formats, and the attachment header;
  - Codex sub-agents left out;
  - a partial or broken line skipped;
  - ids refused unless listed, so no paths;
  - `since`;
  - a long file keeping its first request;
  - the length caps.
- **Real data:** `cargo test real_sessions -- --ignored --nocapture` lists
  and reads the files on this PC.
- **Settings:** an older settings file keeps the switch off.
- **JS:** the note, the switch calling its own command, and source
  contracts showing both tools check the switch first and taint the
  conversation.

## Not done yet

- Automatic summaries when an agent finishes. This would be a separate
  opt-in watcher, with deduplication and a rule for when to notify.
