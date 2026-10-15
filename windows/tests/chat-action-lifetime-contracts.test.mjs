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
const wiring = native("lib.rs");

test("native proposals carry request ownership independently of the grant", () => {
  assert.match(tools, /pub struct Proposal\s*\{[\s\S]*pub request_id: Option<String>/);
  assert.match(tools, /Proposal \{ action_id: id\.clone\(\), request_id: request_id\.map\(str::to_owned\), kind, label, detail, grant \}/);
  assert.match(tools, /intent_id: intent\.to_string\(\)/);
});

test("native reset retires action authority before resetting model history", () => {
  assert.match(wiring, /let closed = actions\.retire\(&turn\.scope\);\s*let _ = turn\.stop\.send\(\(\)\)/);
  assert.match(wiring, /let closed = Self::cancel_active\(&mut active, actions\);\s*after\(\)/);
  assert.match(wiring, /fn chat_reset\([^\n]+\{\s*assistant::tools::close_cards\(&app, stop\.cancel_with\(&actions, \|\| chat\.reset\(\)\)\)/);
  assert.match(wiring, /tokio::select! \{\s*biased;\s*_ = cancelled => None,\s*reply = reply => Some\(reply\)/);
});

test("chat browser and launch tools pass their scope through the final OS worker gate", () => {
  assert.match(tools, /super::browser::switch_if\(a\.window, a\.tab_id, a\.title, move \|\| scope\.try_start\(\)\)/);
  assert.match(tools, /super::launch::open_if\(action_id\.clone\(\), target, move \|\| scope\.try_start\(\)\)/);
  assert.match(browser, /pub fn switch_if\([\s\S]*with_uia\(move \|ua\| \{[\s\S]*confirmed_tab\(&all, window, &id, &title\)\?[\s\S]*if !start\(\) \{\s*return Err\("The chat was stopped\."\.into\(\)\);\s*\}\s*unsafe \{ pattern\.Select\(\) \}/);
});

test("both proposed and freshly resolved tabs reject same-window title ambiguity", () => {
  assert.match(tools, /candidate\.window == tab\.window && candidate\.title == tab\.title\)\.count\(\) == 1/);
  assert.match(tools, /let Some\(tab\) = proposed_tab\(&all, &args\)\.cloned\(\)/);
  assert.match(browser, /tab\.window == window && tab\.title == title\)\.count\(\) != 1/);
  assert.match(browser, /return Err\("the tab title is ambiguous"\.into\(\)\)/);
});
