// The assistant contracts (src/assistant/contracts.ts) against the fixture
// the Rust side reads too, and the wall between the assistant and the agents'
// permission requests.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import {
  ACTION_KINDS, CONTRACT_VERSION, VOICE_ERRORS, VOICE_EVENT, VOICE_STATES, parseVoiceEvent,
} from "../src/assistant/contracts.ts";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/assistant-contracts.json", import.meta.url), "utf8"),
);

test("the registry, states and version match the shared fixture", () => {
  assert.equal(CONTRACT_VERSION, fixture.version);
  assert.equal(VOICE_EVENT, fixture.voiceEvent);
  assert.deepEqual(ACTION_KINDS, fixture.actionKinds);
  assert.deepEqual(VOICE_STATES, fixture.voiceStates);
  assert.deepEqual(VOICE_ERRORS, fixture.voiceErrors);
});

test("voice events are accepted or dropped as the fixture says", () => {
  for (const { name, valid, event } of fixture.voiceEvents) {
    const parsed = parseVoiceEvent(event);
    if (valid) assert.deepEqual(parsed, event, name);
    else assert.equal(parsed, null, name);
  }
});

test("anything that isn't an object is not a voice event", () => {
  for (const raw of [null, undefined, "listening", 1, []]) assert.equal(parseVoiceEvent(raw), null);
});

test("unknown fields are not carried into the island", () => {
  const parsed = parseVoiceEvent({ v: 1, captureId: "c", state: "idle", requestId: "1", decision: "allow" });
  assert.deepEqual(parsed, { v: 1, captureId: "c", state: "idle" });
});

function sources(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? sources(path) : /\.ts$/.test(name) ? [path] : [];
  });
}

test("the assistant's code never reaches agent permissions", () => {
  const files = sources(new URL("../src/assistant", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
  assert.ok(files.length >= 1);
  for (const file of files) {
    const text = readFileSync(file, "utf8")
      // Comments may explain the rule; only code is held to it.
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/(^|[^:])\/\/.*$/gm, "$1");
    for (const word of ["approvalDecision", "approvalAnswer", "approvalAck", "approvalDecline", "approval_", "PermissionRequest"]) {
      assert.ok(!text.includes(word), `${file} mentions ${word}`);
    }
  }
});
