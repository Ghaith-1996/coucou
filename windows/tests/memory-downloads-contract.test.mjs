// Executable source contracts, not Windows/native execution. Filesystem and
// resolver-failure behavior are tested beside the Rust implementation.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../src-tauri/src/memory.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");

test("memory export resolves Downloads through Tauri's native known-folder path", () => {
  assert.match(source, /use tauri::Manager;/);
  assert.match(source, /pub fn export\(webview: &tauri::Webview\)/);
  assert.match(source, /export_using\(\|\| webview\.path\(\)\.download_dir\(\)\.map_err\(\|e\| e\.to_string\(\)\), &stem, &memory\)/);
  assert.doesNotMatch(source, /home_dir\(\)|USERPROFILE|join\("Downloads"\)/);
});

test("lookup failure cannot create an export in a guessed fallback folder", () => {
  assert.match(source, /fn export_using\([\s\S]*?let dir = resolve_downloads\(\)\?;\s*std::fs::create_dir_all\(&dir\)\.map_err\(\|e\| e\.to_string\(\)\)\?;\s*export_to\(&dir, stem, memory\)/);
  assert.match(source, /pub fn export\([^\n]+\{\s*let memory = with\(\|m\| Ok\(m\.clone\(\)\)\)\?;/);
  assert.match(source, /pub fn memory_export\(webview: tauri::Webview\)[^\n]*\{\s*from_settings\(&webview\)\?;\s*let file = export\(&webview\)\?;/);
  assert.match(source, /OpenOptions::new\(\)\.write\(true\)\.create_new\(true\)\.open\(path\)\?/);
});
