import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, sent, internals } from "./tauri.mjs";
import { voiceSection } from "../src/settings/voice.ts";

installFakeDom();
const base = { id: "base", label: "Whisper base", file: "ggml-base.bin", installed: false, size: 147951465, source: "https://example.com/model" };
const turbo = { ...base, id: "large-v3-turbo-q5_0", label: "Whisper turbo", file: "ggml-turbo.bin" };
const status = { supported: true, selected: "base", models: [base, turbo], downloading: null, received: 0 };
beforeEach(() => { calls.length = 0; });

test("unsupported platforms do not show or enable model controls even if a model exists", () => {
  assert.equal(voiceSection({ ...status, supported: false, models: [{ ...base, installed: true }] }), null);
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
  assert.match(section.textContent, /Download \(148 MB\)/);
  assert.equal(sent("voice_model_download").length, 0, "rendering never downloads");
});

test("supported platforms show installed model controls", () => {
  const section = voiceSection({ ...status, models: [{ ...base, installed: true }] });
  assert.match(section.textContent, /Captures are transcribed on this computer/);
  assert.match(section.textContent, /Delete the model/);
});


test("deleting the selected model displays the installed fallback returned by Rust", async (t) => {
  const initial = { ...status, selected: turbo.id, models: [{ ...base, installed: true }, { ...turbo, installed: true }] };
  const fallback = { ...status, models: [{ ...base, installed: true }, turbo] };
  const original = internals.invoke;
  t.mock.method(internals, "invoke", async (cmd, args) => {
    if (cmd === "voice_model_delete") {
      calls.push([cmd, args]);
      return fallback;
    }
    return original(cmd, args);
  });
  const section = voiceSection(initial);
  const row = section.find("label").find((label) => label.textContent === "Whisper turbo").parentNode.parentNode;
  row.find("button").find((button) => button.textContent === "Delete the model").fire("click");
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(sent("voice_model_delete"), [{ id: turbo.id }]);
  const baseRow = section.find("label").find((label) => label.textContent === "Whisper base").parentNode.parentNode;
  const turboRow = section.find("label").find((label) => label.textContent === "Whisper turbo").parentNode.parentNode;
  assert.match(baseRow.textContent, /In use/);
  assert.doesNotMatch(turboRow.textContent, /In use/);
  assert.match(section.textContent, /Captures are transcribed on this computer/);
  assert.equal(sent("voice_model_download").length, 0);
});
