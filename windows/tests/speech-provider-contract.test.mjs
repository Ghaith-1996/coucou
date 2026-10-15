// Source-level IPC/privacy guards complement speech.rs's native lifecycle
// tests. They can also run where the Windows Rust toolchain is unavailable.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const wiring = read("../src-tauri/src/lib.rs");
const speech = read("../src-tauri/src/assistant/speech.rs");
const chatgpt = read("../src-tauri/src/chatgpt.rs");
function body(source, name) {
  const start = source.search(new RegExp(`\\bfn ${name}\\(`));
  assert.notEqual(start, -1, `${name} exists`);
  return source.slice(start, source.indexOf("\n}", start) + 2);
}

test("speech binds the optional IPC identity to the exact native chat settings snapshot", () => {
  const send = body(wiring, "chat_send");
  assert.match(send, /request_id: Option<String>/);
  assert.match(send, /let settings = shared\.settings\.lock\(\)\.unwrap\(\)\.clone\(\)/);
  assert.match(send, /speech\.begin\(request_id\.as_deref\(\), &settings\)/);
  assert.match(send, /chat::send\(&app, &chat, &settings,/);
  assert.ok(send.indexOf("speech.begin(") < send.indexOf("chat::send("));
  assert.match(send, /if result\.is_err\(\)\s*\{\s*speech\.failed\(speech_turn\.as_ref\(\)\)/);
});

test("speech requires a matching native turn and never reads the mutable provider picker", () => {
  const say = body(speech, "speech_say");
  assert.match(say, /request_id: Option<String>/);
  assert.match(say, /speech\.acquire\(request_id\.as_deref\(\)\)\?/);
  assert.doesNotMatch(say, /shared|settings\.lock|chat_provider|provider:|allow_remote:/);
  assert.match(say, /if turn\.remote && crate::chatgpt::speech_available\(\)\.await\s*\{\s*turn\.require_current\(\)\?/);
  assert.match(say, /chatgpt::speak\(&text, \|\| turn\.require_current\(\)\)/);
  assert.match(say, /Ok\(mp3\) => \{\s*turn\.require_current\(\)\?/);
  assert.match(say, /turn\.require_current\(\)\?;\s*#\[cfg\(windows\)\]/);
});

test("stop and navigation revoke speech while completed playback has a scoped end command", () => {
  for (const command of ["chat_stop", "chat_reset", "chat_history_open", "chat_undo_last"]) {
    assert.match(body(wiring, command), /speech\.clear\(\)/, command);
  }
  assert.match(wiring, /\.manage\(assistant::speech::Turns::default\(\)\)/);
  assert.match(wiring, /assistant::speech::speech_end,/);
  const end = body(speech, "speech_end");
  assert.match(end, /webview\.label\(\) != crate::island::WINDOW_LABEL/);
  assert.match(end, /speech\.end\(&request_id\)/);
  assert.doesNotMatch(end, /\.clear\(/);
  assert.match(body(speech, "failed"), /Arc::ptr_eq\(current, turn\)/);
});

test("remote speech rechecks native authorization after token work and on each send retry", () => {
  const speak = body(chatgpt, "speak");
  assert.match(speak, /authorize: impl Fn\(\) -> Result<\(\), String> \+ Send \+ Sync/);
  assert.match(speak, /plan_ready\(\)\.await\?;\s*authorize\(\)\?/);
  const retry = speak.slice(speak.indexOf("for attempt in 0..2"));
  assert.match(retry, /authorize\(\)\?;\s*let sent = request\.send\(\)\.await;\s*authorize\(\)\?/);
  assert.match(retry, /access_token\(true, &account\)\.await\?;\s*continue/);
  assert.ok(speak.indexOf("access_token(false, &account).await?") < speak.indexOf("request.send().await"));
  assert.match(speak, /lock\(\)\.await;\s*authorize\(\)\?;\s*check_session_generation/);
});

test("speech binding shares the native cancellation and replacement lock", () => {
  const send = body(wiring, "chat_send");
  assert.match(send, /stop\.begin_with\(&actions, \|\| speech\.clear\(\)\)/);
  assert.match(send, /stop\.with_current\(&scope, \|\| speech\.begin\(request_id\.as_deref\(\), &settings\)\)/);
  assert.match(wiring, /fn with_current<T>[\s\S]*let active = self\.0\.lock\(\)\.unwrap\(\);\s*active\.as_ref\(\)\.filter\(\|turn\| turn\.scope\.same_turn\(scope\)\)\.map\(\|_\| action\(\)\)/);
  assert.match(body(wiring, "chat_stop"), /cancel_with\(&actions, \|\| speech\.clear\(\)\)/);
});
