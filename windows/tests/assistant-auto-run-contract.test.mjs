// "Do it without asking" is the user's switch, owned by Rust: only Settings
// turns it on, a page saving its settings can't, and Rust reads it again
// itself before anything runs. These source contracts keep that wiring
// checked everywhere; the behaviour is tested beside the Rust code.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8").replace(/\r\n/g, "\n");
const native = (file) => read(`src-tauri/src/${file}`).split("#[cfg(test)]")[0];

test("only the Settings window can turn the switch on, through its own command", () => {
  const tools = native("assistant/tools.rs");
  assert.match(tools, /pub fn assistant_set_auto_run\([^)]*webview: tauri::Webview[^)]*\)[^{]*\{\s*set_owned\(&app, &webview, \|s\| s\.assistant_auto_run = enabled\)\?;\s*set_auto_run\(enabled\);/);
  assert.match(tools, /fn set_owned\([^)]*\)[^{]*\{\s*if webview\.label\(\) != "settings" \{\s*return Err/);
  const settings = read("src/settings/main.ts");
  assert.match(settings, /Bridge\.assistantSetAutoRun\(v\)/);
  assert.doesNotMatch(settings, /settings\.assistantAutoRun = v;\s*void save\(\)/, "never through saveSettings");
});

test("reading conversations is its own switch, also Settings-only, checked before any read", () => {
  const tools = native("assistant/tools.rs");
  assert.match(tools, /pub fn assistant_set_read_chats\([^)]*\)[^{]*\{\s*set_owned\(&app, &webview, \|s\| s\.assistant_read_chats = enabled\)\?;/);
  assert.match(tools, /fn set_owned\([^)]*\)[^{]*\{\s*if webview\.label\(\) != "settings" \{\s*return Err/);
  for (const name of ["list_sessions", "read_session", "read_window"]) {
    assert.match(tools, new RegExp(`async fn ${name}\\([^)]*\\)[^{]*\\{\\s*if !reading_chats_on\\(app\\) \\{\\s*return result\\(READ_CHATS_OFF\\);`));
  }
  // Checked again once the read finished: turning it off mid-read sends nothing.
  for (const name of ["list_sessions", "read_session"]) {
    const start = tools.indexOf(`async fn ${name}(`);
    const body = tools.slice(start, tools.indexOf("\n}\n", start));
    assert.equal(body.match(/if !reading_chats_on\(app\)/g)?.length, 2, name);
  }
  // What the agents said is outside text: it taints the conversation.
  assert.equal(tools.match(/taint_conversation\(app\);\s*let _ = app\.emit_to\(crate::island::WINDOW_LABEL, USED_EVENT, ToolUsed \{ request_id, tool: "agent-sessions"/g)?.length, 2);
  assert.match(tools, /Ok\(Some\(window\)\) => \{\s*taint_conversation\(app\);\s*let _ = app\.emit_to\(crate::island::WINDOW_LABEL, USED_EVENT, ToolUsed \{ request_id, tool: "chat-window"/);
  assert.match(read("src/settings/main.ts"), /Bridge\.assistantSetReadChats\(v\)/);
});

test("the window reader skips what is being typed and secrets, and reads only the two chat apps", () => {
  const window = native("assistant/chat_window.rs");
  assert.match(window, /if !\(password \|\| kind == UIA_EditControlTypeId \|\| kind == UIA_ComboBoxControlTypeId\) \{/);
  assert.match(window, /pid != std::process::id\(\) && exe_path\(pid\)\.as_deref\(\)\.and_then\(chat_app_of\) == Some\(search\.app\)/);
  assert.match(window, /COINIT_MULTITHREADED/);
});

test("a page saving its settings keeps Rust's value", () => {
  const lib = native("lib.rs");
  assert.match(lib, /fn save_settings\([\s\S]*settings\.assistant_auto_run = current\.assistant_auto_run;/);
  assert.match(lib, /fn save_settings\([\s\S]*settings\.assistant_read_chats = current\.assistant_read_chats;/);
  assert.match(lib, /assistant::tools::set_auto_run\(loaded\.assistant_auto_run\);/);
});

test("voice asks Rust to launch without a click only as automatic, which Rust checks", () => {
  const app = native("assistant/app.rs");
  assert.match(app, /if auto && !super::tools::auto_run_on\(\) \{\s*return Err\(ErrorCode::Unavailable\);/);
  assert.match(app, /Some\(app\) if auto => \{\s*let gate = super::tools::ActionScope::new\(\)\.gated\(super::tools::auto_run_on\);/);
  const voice = read("src/assistant/voice.ts");
  assert.match(voice, /if \(Voice\.app\.status === "ready" && State\.settings\.assistantAutoRun\) void voiceOpen\(true\);/);
});

test("outside text taints the conversation before the model reads it", () => {
  const tools = native("assistant/tools.rs");
  assert.match(tools, /taint_conversation\(app\);\s*let _ = app\.emit_to\(crate::island::WINDOW_LABEL, USED_EVENT, ToolUsed \{ request_id, tool: "browser-tabs"/);
  const chat = native("chat.rs");
  assert.match(chat, /if context\.is_some\(\) \{\s*chat\.taint\(\);\s*\}/);
  assert.match(chat, /\*c = Conversation \{ epoch, plain, tainted: true, \.\.Default::default\(\) \};/);
  assert.match(chat, /let remember_asked = crate::assistant::tools::asks_to_remember\(&query\);/);
});
