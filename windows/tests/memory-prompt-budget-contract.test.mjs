// Executable source contracts only. Unicode/boundary behavior is covered by
// native tests beside prompt_block in memory.rs; these checks do not run Rust.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
const source = readFileSync(new URL("../src-tauri/src/memory.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");
const prompt = source.slice(source.indexOf("pub fn prompt_block"), source.indexOf("pub enum Unreadable"));

test("memory prompt counts the prefix and whole lines in Unicode scalar values", () => {
  assert.match(source, /pub const MAX_PROMPT_CHARS: usize = 6000;/);
  assert.match(prompt, /let mut used_chars = block\.chars\(\)\.count\(\);\s*for item in &self\.items \{\s*let line = format!\("\\n- \{\}", item\.text\);\s*let line_chars = line\.chars\(\)\.count\(\);/);
  assert.doesNotMatch(prompt, /block\.len\(\)|line\.len\(\)/);
});

test("exactly the character limit is accepted and overflow omits a whole item", () => {
  assert.match(prompt, /if used_chars \+ line_chars > MAX_PROMPT_CHARS \{\s*break;\s*\}\s*block\.push_str\(&line\);\s*used_chars \+= line_chars;/);
  assert.match(prompt, /if !self\.enabled \|\| self\.items\.is_empty\(\) \{\s*return None;/);
});
