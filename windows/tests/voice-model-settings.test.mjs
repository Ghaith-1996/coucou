import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, sent } from "./tauri.mjs";
import { voiceSection } from "../src/settings/voice.ts";

installFakeDom();
const status = { supported: true, installed: false, downloading: false, received: 0, size: 147951465, source: "https://example.com/model" };
beforeEach(() => { calls.length = 0; });

test("unsupported platforms do not show or enable model controls even if a model exists", () => {
  assert.equal(voiceSection({ ...status, supported: false, installed: true }), null);
  assert.equal(sent("voice_model_download").length, 0);
  assert.equal(sent("voice_model_delete").length, 0);
});

test("unknown backend capability does not advertise local transcription", () => {
  assert.equal(voiceSection(null), null);
  const { supported, ...unknown } = status;
  assert.equal(voiceSection(unknown), null);
});

test("supported platforms keep the explicit model download button", () => {
  const section = voiceSection(status);
  assert.match(section.textContent, /Speech recognition runs on this computer/);
  assert.match(section.textContent, /Download the model/);
  assert.equal(sent("voice_model_download").length, 0, "rendering never downloads");
});

test("supported platforms show installed model controls", () => {
  const section = voiceSection({ ...status, installed: true });
  assert.match(section.textContent, /Captures are transcribed on this computer/);
  assert.match(section.textContent, /Delete the model/);
});
