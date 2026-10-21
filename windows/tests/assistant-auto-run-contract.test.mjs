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
  assert.match(tools, /pub fn assistant_set_auto_run\([^)]*webview: tauri::Webview[^)]*\)[^{]*\{\s*if webview\.label\(\) != "settings" \{\s*return Err/);
  assert.match(tools, /current\.assistant_auto_run = enabled;[\s\S]*set_auto_run\(enabled\);/);
  const settings = read("src/settings/main.ts");
  assert.match(settings, /Bridge\.assistantSetAutoRun\(v\)/);
  assert.doesNotMatch(settings, /settings\.assistantAutoRun = v;\s*void save\(\)/, "never through saveSettings");
});

test("a page saving its settings keeps Rust's value", () => {
  const lib = native("lib.rs");
  assert.match(lib, /fn save_settings\([\s\S]*settings\.assistant_auto_run = current\.assistant_auto_run;/);
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
