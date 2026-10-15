// Native execution needs Windows/Rust. These source contracts additionally keep
// the frontend/native ownership and final worker-gate wiring checked everywhere;
// the deterministic native behavioral tests live beside the Rust implementation.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const native = file => readFileSync(new URL(`../src-tauri/src/${file}`, import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");
const tools = native("assistant/tools.rs");
const browser = native("assistant/browser.rs");
const launch = native("assistant/launch.rs");
const wiring = native("lib.rs");

test("native proposals carry request ownership independently of the grant", () => {
  assert.match(tools, /pub struct Proposal\s*\{[\s\S]*pub request_id: Option<String>/);
  assert.match(tools, /Proposal \{ action_id: id\.clone\(\), request_id: request_id\.map\(str::to_owned\), kind, label, detail, grant \}/);
  assert.match(tools, /intent_id: intent\.to_string\(\)/);
});

test("native reset retires action authority before resetting model history", () => {
  assert.match(wiring, /let closed = actions\.retire\(&turn\.scope\);\s*let _ = turn\.stop\.send\(\(\)\)/);
  assert.match(wiring, /let closed = Self::cancel_active\(&mut active, actions\);\s*after\(\)/);
  assert.match(wiring, /fn chat_reset\([^\n]+\{\s*assistant::tools::close_cards\(&app, stop\.cancel_with\(&actions, \|\| \{\s*speech\.clear\(\);\s*chat\.reset\(\);\s*\}\)\)/);
  assert.match(wiring, /tokio::select! \{\s*biased;\s*_ = cancelled => None,\s*reply = reply => Some\(reply\)/);
});

test("chat browser and launch tools pass their scope through the final OS worker gate", () => {
  assert.match(tools, /super::browser::switch_if\(a\.window, a\.tab_id, a\.title, Some\(scope\)\)/);
  assert.match(tools, /super::launch::open_if\(action_id\.clone\(\), target, Some\(scope\)\)/);
  assert.match(browser, /pub fn switch_if\([^\n]*scope: Option<ActionScope>[^\n]*\{\s*with_uia\(move \|ua\| \{/);
});

test("native cancellation and immediate effects hold the same scope lock", () => {
  assert.match(tools, /pub\(crate\) fn run_if_active<T>\([^\n]*\{\s*let active = self\.0\.lock\(\)\.unwrap\(\);\s*if !\*active \{\s*return None;\s*\}\s*let result = effect\(\);\s*drop\(active\);\s*Some\(result\)/);
  assert.doesNotMatch(tools, /fn try_start\(/);
  assert.match(tools, /pub fn retire\([^\n]*\{\s*let mut active = scope\.0\.lock\(\)\.unwrap\(\);\s*\*active = false/);
});

test("browser effect locking covers selection and foreground changes but excludes observation", () => {
  assert.match(browser, /confirmed_tab\(&all, window, &id, &title\)\?[\s\S]*let select = \|\| -> Result<\(\), String> \{\s*unsafe \{ pattern\.Select\(\) \}[\s\S]*ShowWindow\(hwnd, SW_RESTORE\)[\s\S]*SetForegroundWindow\(hwnd\)[\s\S]*Ok\(\(\)\)\s*\};\s*match scope \{\s*Some\(scope\) => scope\.run_if_active\(select\)[\s\S]*None => select\(\)\?,\s*\}\s*std::thread::sleep/);
  assert.match(browser, /switch_if\(window, id, title, None\)/);
  assert.match(browser, /Ok\(switch_succeeded\(is_selected, tab\.window, foreground\)\)/);
});

test("shell effect locking ends before its ten-second evidence wait", () => {
  assert.match(launch, /let mut execute = \|\| unsafe \{ ShellExecuteExW\(&mut info\) \};\s*let executed = match scope \{\s*Some\(scope\) => scope\.run_if_active\(execute\),\s*None => Some\(execute\(\)\),\s*\};/);
  assert.ok(launch.indexOf("let executed = match scope") < launch.indexOf("while started.elapsed() < WAIT"));
  const observe = launch.slice(launch.indexOf("while started.elapsed() < WAIT"));
  assert.doesNotMatch(observe, /run_if_active|scope\.|\.lock\(/);
});

test("both proposed and freshly resolved tabs reject same-window title ambiguity", () => {
  assert.match(tools, /candidate\.window == tab\.window && candidate\.title == tab\.title\)\.count\(\) == 1/);
  assert.match(tools, /let Some\(tab\) = proposed_tab\(&all, &args\)\.cloned\(\)/);
  assert.match(browser, /tab\.window == window && tab\.title == title\)\.count\(\) != 1/);
  assert.match(browser, /return Err\("the tab title is ambiguous"\.into\(\)\)/);
});

test("remember carries native turn ownership through approval and the storage lock", () => {
  assert.match(tools, /"remember" => remember\(app,[^\n]+intent, request_id, scope\)\.await/);
  assert.match(tools, /ActionKind::MemorySave,[^\n]+intent, request_id, scope\)\.await/);
  assert.match(tools, /let saved = scope\.run_if_active\(\|\| \{\s*crate::memory::with\(\|m\| m\.add/);
});

test("web approval and the actual URL launch worker retain native turn ownership", () => {
  const launch = native("assistant/launch.rs");
  assert.match(tools, /"open_web" => open_web\(app,[^\n]+intent, request_id, scope\)\.await/);
  assert.match(tools, /ActionKind::OpenUrl,[^\n]+intent, request_id, scope\)\.await/);
  assert.match(tools, /open_url_if\(action_id\.clone\(\), a\.url, Some\(scope\)\)/);
  assert.match(launch, /pub async fn open_url_if\([^\n]*scope: Option<ActionScope>[^\n]*\{\s*on_launch_thread\(move \|\| \{[\s\S]*let mut execute = \|\| unsafe \{ ShellExecuteExW\(&mut info\) \};\s*let executed = match scope \{\s*Some\(scope\) => scope\.run_if_active\(execute\),\s*None => Some\(execute\(\)\),\s*\};/);
});
