// Executable source contracts only: these inspect Rust wiring, not native
// persistence or Tauri event delivery. Settings listener behavior is covered
// separately by the frontend tests.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const native = (path) => readFileSync(new URL(path, import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");
const tools = native("../src-tauri/src/assistant/tools.rs");
const memory = native("../src-tauri/src/memory.rs");
const rememberStart = tools.indexOf("async fn remember(");
assert.notEqual(rememberStart, -1);
const rememberEnd = tools.indexOf("\n}", rememberStart);
assert.notEqual(rememberEnd, -1);
const remember = tools.slice(rememberStart, rememberEnd + 2);

test("remember invalidates only Settings without transmitting memory text", () => {
  assert.equal((tools.match(/"memory-changed"/g) ?? []).length, 1);
  assert.match(remember, /app\.emit_to\("settings", "memory-changed", \(\)\)/);
  assert.doesNotMatch(remember, /app\.emit\(/);
});

test("remember notifies only the successful persisted result outside the action lock", () => {
  assert.match(remember, /let saved = scope\.run_if_active\(\|\| \{\s*crate::memory::with\(\|m\| m\.add\(&a\.text, crate::memory::Source::Chat, crate::memory::now\(\)\)\)\s*\}\)\.unwrap_or_else\(\|\| Err\("The chat was stopped\."\.into\(\)\)\)/);
  assert.match(remember, /match saved \{\s*Ok\(_\) => \{\s*let _ = app\.emit_to\("settings", "memory-changed", \(\)\);\s*result\("Saved to the user's memory\."\)\s*\},?\s*Err\(why\) => result\(format!\("Not saved: \{why\}"\)\),\s*\}/);
  assert.ok(remember.indexOf("let saved = scope.run_if_active") < remember.indexOf("match saved"));
  // Retirement after a completed save cannot undo it or suppress its refresh.
  assert.doesNotMatch(remember.slice(remember.indexOf("match saved")), /scope\./);
});

test("the notification's success value follows save and rollback handling", () => {
  assert.match(memory, /pub fn with<T>[^\n]*\{\s*let mut store = STORE\.lock\(\)\.unwrap\(\);\s*with_store\(&mut store, &path\(\), f\)\s*\}/);
  assert.match(memory, /if \*memory != before \{\s*if let Err\(err\) = save_to\(file, memory\) \{\s*\*memory = before;\s*return Err\(err\);\s*\}\s*\}\s*Ok\(out\)/);
  assert.match(remember, /confirm\(app, ActionKind::MemorySave,[^\n]+scope\)\.await\s*else \{\s*return result\("The user declined: nothing was remembered\."\);\s*\};\s*let saved = scope\.run_if_active/);
});
