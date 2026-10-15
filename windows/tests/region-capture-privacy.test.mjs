// Runnable source-contract guards; the fault-injected Rust tests in capture.rs
// exercise the actual guard and still require a Windows Rust test runner.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const read = (name) => readFileSync(new URL(`../src-tauri/src/assistant/region/${name}.rs`, import.meta.url), "utf8");
const capture = read("capture"), commands = read("commands");

test("a missing required Coucou HWND fails the capture instead of being skipped", () => {
  const ours = commands.slice(commands.indexOf("let ours:"), commands.indexOf("let on_screen"));
  assert.match(ours, /Option<Vec<isize>>/);
  assert.doesNotMatch(ours, /filter_map/);
  assert.match(commands, /capture::excluding\(ours\?\.into_iter\(\)/);
});

test("settle and pixel copying run inside the serialized exclusion operation", () => {
  assert.match(commands, /capture::excluding\([\s\S]*?\|\| \{\s*std::thread::sleep\(SETTLE\);\s*capture::grab\(on_screen\)/);
  assert.match(capture, /static CAPTURE_LOCK: Mutex<\(\)> = Mutex::new\(\(\)\)/);
  assert.match(capture, /with_excluded\(&CAPTURE_LOCK, windows,/);
  assert.match(capture, /let _serial = lock\.lock\(\)\.ok\(\)\?;\s*let mut excluded = Excluded/);
});

test("every required window exclusion succeeds before copying, without a new read precondition", () => {
  assert.match(capture, /if !set\(window, WDA_EXCLUDEFROMCAPTURE\.0\) \{\s*return None;/);
  assert.doesNotMatch(capture, /GetWindowDisplayAffinity\(/);
  assert.doesNotMatch(commands, /IsWindowVisible|is_visible/);
});

test("RAII cleanup preserves existing restore-to-NONE semantics, including partial failure", () => {
  assert.match(capture, /excluded\.windows\.push\(window\);\s*if !set/);
  assert.match(capture, /for &window in self\.windows\.iter\(\)\.rev\(\)/);
  assert.match(capture, /\(self\.set\)\(window, WDA_NONE\.0\)/);
});
