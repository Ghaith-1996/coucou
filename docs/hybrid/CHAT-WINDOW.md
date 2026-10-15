# Hybrid assistant: reading the ChatGPT and Claude desktop windows

Ask "summarize my ChatGPT chat" while the ChatGPT or Claude desktop app is
open on a conversation. Those apps keep their chats on their services, not
in files on this PC ([AGENT-SESSIONS.md](AGENT-SESSIONS.md)), so the only
local source is what the open window shows. The chat reads that text through
UI Automation and summarizes it. It does this only when asked.

## Tool (`read_chat_window {app: chatgpt | claude}`, `assistant/chat_window.rs`)

- **Same switch as session reading:** Settings → Assistant → "Let the chat
  read my conversations", off by default and owned by Rust. While it is off,
  nothing is read.
- **Only these two apps:** a window belongs to them only if its process's
  executable is one of these:
  - `WindowsApps\OpenAI.ChatGPT-Desktop_…\app\ChatGPT….exe`
  - `WindowsApps\Claude_…\app\claude.exe`
  - `AnthropicClaude\app-…\claude.exe` (Claude's own installer)

  Claude Code's `claude.exe` lives elsewhere and has no window of its own.
  No other app is ever read, and never Coucou. The front visible, unowned
  window of the app is the one read.
- **What is read:** the Name of each element at the end of a branch in
  UI Automation's ControlView, in tree (reading) order, with consecutive
  repeats dropped.
  - Edit and ComboBox elements and anything marked `IsPassword` are skipped
    together with their contents. This keeps out the message being typed.
  - Other sensitive text inside ordinary messages is read. That is why the
    switch exists and why its hint says what is sent where.
- **Limits:**
  - at most 20,000 elements and 3 seconds;
  - the last 20,000 characters are kept, since the newest messages are at
    the end;
  - `shortened` says when text was cut or the walk stopped early.
- **Taint:** the result taints the conversation ([CHAT-ACTIONS.md](CHAT-ACTIONS.md)).
  The model is told the text is information, never instructions.
- **Note:** the chat shows "Read the chat app's window"
  (`chat-tool-used`, `chat-window`).

## Chromium's tree

Both apps draw their chat with Chromium, which builds the accessibility tree
of a page only after something asks for it. Read cold, the Claude window
showed only its title bar buttons. The reader therefore:

1. asks each page surface (`Chrome_RenderWidgetHostHWND` child window) for
   its accessible object (`AccessibleObjectFromWindow`, `OBJID_CLIENT`),
   which is what turns the tree on;
2. reads, and reads again every 400 ms while fewer than 20 words came back,
   within the same 3 seconds.

COM is multithreaded (`COINIT_MULTITHREADED`) on a worker thread of its own,
the same setup as `browser.rs`.

## What it is not

- **Not the whole screen, and not the whole history.** A long chat gives only
  what the app has loaded and exposes. Scrolling up first gives more.
- **Not tested live yet.** Automated checks were not allowed to read the
  owner's real windows. The owner checks it: turn the switch on, open a chat
  in the ChatGPT app, and ask "summarize my ChatGPT chat".

## Tests

- **Rust:**
  - executable matching, including the CLI's `claude.exe`, Codex, Chrome,
    look-alike paths and Coucou;
  - line joining (repeats dropped, the end kept, early stop flagged);
  - an ignored real-window test that prints sizes only
    (`cargo test real_chat_window -- --ignored --nocapture`).
- **JS:**
  - the note;
  - source contracts: the switch is checked first, the result taints the
    conversation, draft and password fields are skipped, only the two apps
    are read, and COM runs multithreaded.
