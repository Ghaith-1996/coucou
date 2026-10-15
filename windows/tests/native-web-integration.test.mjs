// Source contracts only: these do not execute Rust, COM, or ShellExecute.
// Native scope race tests and retired-launch tests live beside their Rust code.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const native = file => readFileSync(new URL(`../src-tauri/src/assistant/${file}.rs`, import.meta.url), "utf8");
const tools = native("tools");
const launch = native("launch");
const contracts = native("contracts");
const urlWorker = launch.slice(launch.indexOf("pub async fn open_url_if("), launch.indexOf("async fn on_launch_thread("));

test("web callers carry native scope to the STA worker and ordinary callers remain unscoped", () => {
  assert.match(tools, /open_url_if\(action_id\.clone\(\), a\.url, Some\(scope\)\)/);
  assert.match(launch, /pub async fn open_url\([^\n]*\{\s*open_url_if\(action_id, url, None\)\.await/);
  assert.match(urlWorker, /scope: Option<ActionScope>[^\n]*\{\s*on_launch_thread\(move \|\| \{/);
  assert.doesNotMatch(tools + launch, /try_start\(|if !start\(\)/);
});

test("URL shell execution is inside the scope lock and cannot claim page-load evidence", () => {
  assert.match(urlWorker, /let mut execute = \|\| unsafe \{ ShellExecuteExW\(&mut info\) \};\s*let executed = match scope \{\s*Some\(scope\) => scope\.run_if_active\(execute\),\s*None => Some\(execute\(\)\),\s*\};\s*match executed \{\s*None => ActionReceipt::failed\(&action_id, ErrorCode::Expired\),\s*Some\(Err\(_\)\) => ActionReceipt::failed\(&action_id, ErrorCode::Failed\),\s*Some\(Ok\(\(\)\)\) => ActionReceipt::unknown\(&action_id\),\s*\}/);
  assert.doesNotMatch(urlWorker, /ActionReceipt::succeeded|sleep\(|while /);
});

test("PR22 keeps strict app matching and Start shortcuts with the merged scope signature", () => {
  assert.match(tools, /let Some\(entry\) = super::apps::find_typed\(&catalog, &args\.name\)\.cloned\(\)/);
  assert.match(launch, /fn open_in_sta\(action_id: &str, app_id: &str, scope: Option<&ActionScope>\) -> ActionReceipt \{\s*(?:\/\/[^\n]*\n\s*)*let file = if super::apps::is_start_shortcut\(app_id\) \{\s*HSTRING::from\(app_id\)\s*\} else \{\s*HSTRING::from\(format!\(r"shell:AppsFolder\\\{app_id\}"\)\)\s*\};/);
});

test("open-url v5 and the current available region shape coexist", () => {
  assert.match(contracts, /pub const CONTRACT_VERSION: u32 = 5;/);
  assert.match(contracts, /pub struct OpenUrlArgs \{\s*pub url: String,\s*\}/);
  assert.match(contracts, /pub struct AskRegionArgs \{\s*pub monitor: String,\s*pub x: i32,\s*pub y: i32,\s*pub width: u32,\s*pub height: u32,\s*\}/);
  const availability = contracts.slice(contracts.indexOf("pub fn available("), contracts.indexOf("pub fn idempotent("));
  for (const kind of ["AskRegion", "MemorySave", "OpenUrl"]) assert.match(availability, new RegExp(`ActionKind::${kind}`));
});

test("web instructions coexist with platform-gated memory without duplicate native tests", () => {
  const variants = tools.match(/const fn instructions_for\(memory_supported: bool\) -> &'static str \{\s*if memory_supported \{\s*"([^"]*)"\s*\} else \{\s*"([^"]*)"\s*\}\s*\}/);
  assert.ok(variants);
  for (const text of variants.slice(1)) {
    assert.match(text, /open web pages and searches in the default browser/);
    assert.match(text, /open files or folders/);
  }
  assert.match(variants[1], /propose a fact for the user's memory/);
  assert.doesNotMatch(variants[2], /memory|remember/);
  assert.match(tools, /assert_eq!\(names, \["list_browser_tabs", "switch_browser_tab", "open_web", "open_app", "list_agent_sessions", "read_chat_window", "read_agent_session"\]\)/);
  const testNames = [...tools.matchAll(/(?:#\[test\]|#\[tokio::test[^\n]*\])\s*(?:async )?fn (\w+)/g)].map(match => match[1]);
  assert.equal(new Set(testNames).size, testNames.length, "auto-merges must not duplicate Rust test definitions");
});
