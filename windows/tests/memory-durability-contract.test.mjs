// Executable source contracts, not native filesystem execution. Fault-injected
// write/sync/rename behavior is tested in memory.rs without requiring DPAPI.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const source = readFileSync(new URL("../src-tauri/src/memory.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");

test("memory flushes encrypted bytes through a File before replacing the store", () => {
  assert.match(source, /fn save_to\([^\n]*\{\s*let plain = serde_json::to_vec\(memory\)[^\n]*;\s*let sealed = seal\(&plain\)\?;[\s\S]*?ensure_private_dir\(dir\)[^\n]*;\s*replace_sealed\(file, &sealed, \|output, bytes\| output\.write_all\(bytes\), \|output\| output\.sync_all\(\)\)/);
  assert.doesNotMatch(source.slice(source.indexOf("fn save_to"), source.indexOf("pub fn with<")), /std::fs::write\(/);
});

test("write and sync failures cannot rename the temp over the previous memory store", () => {
  assert.match(source, /let mut output = std::fs::File::create\(&tmp\)\?;\s*let result = write\(&mut output, sealed\)\.and_then\(\|\(\)\| sync\(&output\)\);\s*drop\(output\);\s*let result = result\.and_then\(\|\(\)\| std::fs::rename\(&tmp, file\)\);\s*if result\.is_err\(\) \{\s*let _ = std::fs::remove_file\(&tmp\);\s*\}\s*result/);
  assert.match(source, /if let Err\(err\) = save_to\(file, memory\) \{\s*\*memory = before;\s*return Err\(err\);\s*\}/);
});
