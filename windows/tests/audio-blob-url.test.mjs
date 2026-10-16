// The app's content security policy plays media only from `self` and `blob:`.
// Rust hands speech over as a `data:` URL, which the built app refused
// ("Speech playback failed" on every clip, found live): the player plays it
// through a blob URL instead, and releases it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { playableUrl } from "../src/assistant/audio-mode.ts";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8").replace(/\r\n/g, "\n");

test("a data URL clip becomes a blob URL with the same bytes and type, released after", async () => {
  const bytes = Uint8Array.from([82, 73, 70, 70, 0, 1, 2, 255]);
  const data = `data:audio/wav;base64,${Buffer.from(bytes).toString("base64")}`;
  const clip = playableUrl(data);
  assert.match(clip.url, /^blob:/);
  const blob = await (await import("node:buffer")).resolveObjectURL(clip.url);
  assert.equal(blob.type, "audio/wav");
  assert.deepEqual(new Uint8Array(await blob.arrayBuffer()), bytes);
  clip.release();
  assert.equal((await import("node:buffer")).resolveObjectURL(clip.url), undefined, "released");
});

test("anything that is not a base64 data URL is played as it is", () => {
  for (const audio of ["wav", "blob:abc", "https://example.com/a.mp3"]) {
    const clip = playableUrl(audio);
    assert.equal(clip.url, audio);
    clip.release();
  }
});

test("the policy still refuses data: media, so the player must convert", () => {
  const conf = JSON.parse(read("src-tauri/tauri.conf.json"));
  const media = conf.app.security.csp.split(";").map((s) => s.trim()).find((s) => s.startsWith("media-src"));
  assert.ok(media.includes("blob:"));
  assert.ok(!media.includes("data:"), "if data: is ever allowed, this conversion can go");
  assert.match(read("src-tauri/src/assistant/speech.rs"), /format!\("data:audio\/(mpeg|wav);base64/);
  assert.match(read("src/assistant/audio-mode.ts"), /const clip = playableUrl\(audio\);\s*const el = new Audio\(clip\.url\);/);
});
