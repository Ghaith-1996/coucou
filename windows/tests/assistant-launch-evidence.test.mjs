// The native launcher is Windows-only. Keep this evidence boundary checked on
// every platform too; the window predicate's behavioral tests live in launch.rs.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../src-tauri/src/assistant/launch.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0]
  .replace(/\/\/.*$/gm, "");
const app = readFileSync(new URL("../src-tauri/src/assistant/app.rs", import.meta.url), "utf8")
  .replace(/\/\/.*$/gm, "");

test("the shell launch uses a dedicated thread instead of a reused blocking worker", () => {
  assert.match(source, /pub async fn open\([^\n]+\{\s*open_if\(action_id, app_id, \|\| true\)\.await/);
  assert.match(source, /pub async fn open_if\([\s\S]*on_launch_thread\(move \|\| \{\s*if !start\(\) \{\s*return ActionReceipt::failed\(&action_id, ErrorCode::Expired\);\s*\}\s*open_in_sta\(&action_id, &app_id\)/);
  assert.match(source, /std::thread::Builder::new\(\)[\s\S]*\.spawn\(move \|\|/);
  assert.match(source, /send\.send\(with_sta\(launch\)\)/);
  assert.match(source, /recv\.await\.map_err/);
  assert.match(app, /Some\(app\) => super::launch::open\(action_id, app\.target\)\.await\?/);
  assert.doesNotMatch(app, /spawn_blocking[\s\S]*super::launch::open/);
});

test("the launch thread enters STA before the shell operation and balances COM on drop", () => {
  assert.match(source, /CoInitializeEx\(None, COINIT_APARTMENTTHREADED \| COINIT_DISABLE_OLE1DDE\)[\s\S]*\.ok\(\)\.map_err/);
  assert.match(source, /let _com = Sta::initialize\(\)\?;\s*Ok\(launch\(\)\)/);
  assert.match(source, /struct Sta\(PhantomData<Rc<\(\)>>\)/);
  assert.match(source, /impl Drop for Sta[\s\S]*CoUninitialize\(\)/);
  assert.match(source, /SEE_MASK_NOCLOSEPROCESS \| SEE_MASK_FLAG_NO_UI \| SEE_MASK_NOASYNC/);
});

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
