# Hybrid assistant — P3 memory

Short facts the user wants every chat to know ("I write in French",
"I use Rust at work"). The user owns them: they can see, edit, delete and
export every item. The chat can only *propose* one, on a card the user
confirms.

## Store (`src-tauri/src/memory.rs`)

- `memory.bin` next to `settings.json`, encrypted with DPAPI for the current
  Windows account (`CryptProtectData`, entropy `coucou-memory-v1`). Another
  account, or the file copied to another PC, can't read it.
- Written whole to `memory.bin.tmp`, then renamed: a crash never leaves half
  a file. The folder is made private like the rest of the config.
- Format: `{ version, enabled, items: [{ id, text, source, createdAt, updatedAt }] }`,
  `source` = `user` (typed in Settings) or `chat` (proposed by the chat and
  confirmed).
- Limits: 200 items, 500 characters each, one line. Line breaks and other
  control characters become spaces, and runs of spaces one. Adding a text
  already there (ignoring ASCII letter case) changes nothing.
- A file that can't be decrypted or parsed, or one written by a newer
  Coucou, is renamed `memory.bin.unreadable-<ms>` and memory starts empty;
  the log says why. If even the rename fails, Coucou refuses to write over
  it.

## In the chat

`chat::system_prompt` appends the items to the instructions of **every
provider** (local models, ChatGPT, Claude, OpenAI-compatible), at most
6,000 characters, unless the switch "Use memory in the chat" is off.
Settings says so: when it is on, memory goes with every question to the
model the user chats with, local or online.

## The `remember` tool (`assistant/tools.rs`)

With Sign in with ChatGPT, the model gets a fourth tool next to the chat
actions ([CHAT-ACTIONS.md](CHAT-ACTIONS.md)):

| Tool | What it does | Confirmation |
|---|---|---|
| `remember` | A `memory-save` action with one line of text | Card: "Remember this?" + the exact text |

- The model is told it is for lasting facts the user shared, never secrets
  or passwords.
- The policy says **ask** for `memory-save` and checks the text like
  `memory::clean` (one line, at most 500 characters).
- After "Do it" and `check_grant`, Rust saves the item with
  `source: chat`. Evidence: `memory:<item id>`. The receipt is logged
  without the text.
- "No", Stop or two minutes without an answer saves nothing.

Contract version 4 adds `memory-save` (idempotent: saving the same text
twice keeps one item).

## Settings → Memory (`src/settings/memory.ts`)

- The switch, a field to add, the list (with a "From the chat" badge),
  Edit / Delete per item, Export and "Delete everything".
- "Delete everything" needs a second click within 4 seconds.
- Export writes a plain JSON copy to `Downloads\Coucou memory <date>.json`
  and shows its path. That copy is **not** encrypted: it is the user's to
  keep.
- The `memory_*` commands answer only the settings window: the island,
  the chat page and the region overlay can't read or change memory.

## Tests

- Rust (`memory.rs`): cleaning, duplicates, the limits, edit/delete, the
  prompt block size, newer and damaged files set aside, DPAPI round trip.
- Policy fixture: a proposed memory is `ask`; a two-line one is
  `invalid-args`.
- JS (`tests/memory-settings.test.mjs`, `tests/chat-actions.test.mjs`):
  listing and the chat badge, add/edit/delete, refusals shown, the
  two-click delete and its timeout, the switch, export, and a
  `memory-save` card saving nothing before "Do it".

## Not done yet

- Not tried live: the model calling `remember` with a real ChatGPT
  account.
- Memory follows the Windows account, not the device: a second PC starts
  empty (export, then add the items there).
