// Native checks here inspect source; they do not execute Rust. Native behavioral
// tests live in tools.rs. The Settings test executes the actual extracted guard
// with a section factory spy, not a duplicate platform implementation.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";

const tools = readFileSync(new URL("../src-tauri/src/assistant/tools.rs", import.meta.url), "utf8")
  .split("#[cfg(test)]")[0].replace(/\/\/.*$/gm, "");
const settings = readFileSync(new URL("../src/settings/main.ts", import.meta.url), "utf8");

test("one native Windows capability controls memory advertising and execution", () => {
  assert.match(tools, /const MEMORY_SUPPORTED: bool = cfg!\(windows\);/);
  assert.match(tools, /pub fn definitions\(\) -> Value \{\s*definitions_for\(MEMORY_SUPPORTED\)\s*\}/);
  assert.match(tools, /if !memory_supported \{\s*definitions\[0\]\["tools"\]\.as_array_mut\(\)\.unwrap\(\)\.retain\(\|tool\| tool\["name"\] != "remember"\);\s*\}/);
  assert.match(tools, /pub const INSTRUCTIONS: &str = instructions_for\(MEMORY_SUPPORTED\);/);
  const instructions = tools.match(/const fn instructions_for\(memory_supported: bool\) -> &'static str \{\s*if memory_supported \{\s*"([^"]*)"\s*\} else \{\s*"([^"]*)"\s*\}\s*\}/);
  assert.ok(instructions, "instructions have supported and unsupported variants");
  assert.match(instructions[1], /propose a fact for the user's memory/);
  assert.doesNotMatch(instructions[2], /memory|remember/);
});

test("stale or direct remember calls are refused before validating text or publishing a proposal", () => {
  assert.match(tools, /async fn remember\([^\n]*\{\s*if let Err\(reason\) = require_memory\(MEMORY_SUPPORTED\) \{\s*return result\(reason\);\s*\}\s*let Ok\(text\) = crate::memory::clean/);
  assert.match(tools, /fn require_memory\(memory_supported: bool\) -> Result<\(\), &'static str> \{\s*memory_supported\.then_some\(\(\)\)\.ok_or\("Memory is only available on Windows\."\)\s*\}/);
  assert.match(tools, /"remember" => remember\(app,[^\n]*intent, request_id, scope\)\.await/);
});

test("Settings constructs Memory only on Windows and keeps the same key-store platform check", () => {
  const platform = settings.match(/const IS_WINDOWS = ([^;]+);/);
  const memory = settings.match(/\.\.\.\((IS_WINDOWS \? \[memorySection\(\)\] : \[\])\)/);
  assert.ok(platform, "Settings has one Windows check");
  assert.ok(memory, "the section factory is inside the Windows branch");
  assert.match(settings, /const KEY_STORE = IS_WINDOWS/);
  for (const [userAgent, expected] of [
    ["Mozilla/5.0 (Windows NT 10.0; Win64; x64)", 1],
    ["Mozilla/5.0 (X11; Linux x86_64)", 0],
    ["Mozilla/5.0 (Macintosh; Intel Mac OS X)", 0],
    ["", 0],
  ]) {
    let constructions = 0;
    const section = {};
    const rendered = runInNewContext(`const IS_WINDOWS = ${platform[1]}; ${memory[1]}`, {
      navigator: { userAgent }, memorySection: () => { constructions++; return section; },
    });
    assert.equal(constructions, expected, userAgent);
    assert.equal(rendered.length, expected, userAgent);
    if (expected) assert.equal(rendered[0], section);
  }
});
