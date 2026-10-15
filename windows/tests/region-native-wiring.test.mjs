// Structural native-wiring checks, not a substitute for the Windows Rust tests.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const commands = readFileSync(new URL("../src-tauri/src/assistant/region/commands.rs", import.meta.url), "utf8");
test("user CloseRequested cancels while intentional teardown bypasses it", () => {
  assert.match(commands, /win\.on_window_event/);
  assert.match(commands, /WindowEvent::CloseRequested/);
  assert.match(commands, /api\.prevent_close\(\)/);
  const teardown = commands.slice(commands.indexOf("fn close_overlays"), commands.indexOf("fn ended"));
  assert.match(teardown, /win\.destroy\(\)/);
  assert.doesNotMatch(teardown, /win\.close\(\)/);
});
test("capture excludes all non-overlay Coucou windows, including Settings", () => {
  const capture = commands.slice(commands.indexOf("let ours:"), commands.indexOf("let on_screen"));
  assert.match(capture, /app\.webview_windows\(\)/);
  assert.match(capture, /!label\.starts_with\(OVERLAY\)/);
  assert.match(capture, /hwnd_of/);
});
test("native ended events identify their session", () => {
  assert.match(commands, /struct Ended/);
  assert.match(commands, /session_id: &'a str/);
  assert.match(commands, /ENDED_EVENT, Ended \{ session_id, reason \}/);
});
test("late cancellation and pick completion tear down only their own overlays", () => {
  const cancel = commands.slice(commands.indexOf("fn cancel_session"), commands.indexOf("/// The island"));
  assert.match(cancel, /if regions\.cancel\(session_id\)/);
  assert.match(cancel, /close_overlays\(app, Some\(session_id\)\)/);
  assert.doesNotMatch(cancel, /close_overlays\(app, None\)/);
  const pick = commands.slice(commands.indexOf("pub async fn region_pick"), commands.indexOf("pub fn region_cancel"));
  assert.match(pick, /close_overlays\(&app, Some\(&session_id\)\)/);
  assert.match(pick, /overlay_index\(&webview, &session_id\)/);
  const teardown = commands.slice(commands.indexOf("fn close_overlays"), commands.indexOf("fn ended"));
  assert.match(teardown, /id == owner/);
});
test("native capture is claimed once before overlay teardown or async screen copying", () => {
  const pick = commands.slice(commands.indexOf("pub async fn region_pick"), commands.indexOf("pub fn region_cancel"));
  const claim = pick.indexOf("regions.claim_pick(&session_id, index)");
  assert.ok(claim >= 0, "the native session must reserve its single capture");
  assert.ok(claim < pick.indexOf("close_overlays"));
  assert.ok(claim < pick.indexOf("spawn_blocking"));
});
