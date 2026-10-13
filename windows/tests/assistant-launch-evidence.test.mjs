// The native launcher is Windows-only. Keep this evidence boundary checked on
// every platform too; the window predicate's behavioral tests live in launch.rs.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../src-tauri/src/assistant/launch.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0]
  .replace(/\/\/.*$/gm, "");

test("an unrelated foreground switch cannot become launch success evidence", () => {
  assert.doesNotMatch(source, /\bGetForegroundWindow\b|foreground:pid:/);
});

test("launch success requires a window of the process returned by the shell", () => {
  assert.match(source, /GetProcessId\(process\)/);
  assert.match(source, /if pid != 0 && has_window\(pid\)/);
  assert.equal((source.match(/ActionReceipt::succeeded\(/g) ?? []).length, 1);
  assert.match(source, /ActionReceipt::succeeded\(action_id, &format!\("window:pid:\{pid\}"\)\)/);
  assert.match(source, /ActionReceipt::unknown\(action_id\)/);
});

test("the launched process handle is owned until evidence polling finishes", () => {
  assert.match(source, /let _process = [\s\S]*OwnedHandle::from_raw_handle\(process\.0\)/);
  assert.doesNotMatch(source, /\bCloseHandle\b|\bdrop\(_process\)/);
});
