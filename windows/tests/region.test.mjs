// Ask about a screen region: the overlay's rectangle in physical pixels
// (src/region/select.ts), and the island's flow (src/assistant/region.ts):
// only the pick this page started is shown, masks stay inside the picture,
// nothing is attached without the click, and the card going away drops it.

import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { between, isRegion, physical } from "../src/region/select.ts";
import {
  MAX_MASKS, Region, addMask, regionAsk, regionCancel, registerRegion, removeMask, resetRegionForTests, startRegion,
} from "../src/assistant/region.ts";

const answers = new Map();
const realInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  await realInvoke(cmd, args);
  if (!answers.has(cmd)) return null;
  const answer = answers.get(cmd);
  if (answer instanceof Error) throw answer;
  return answer;
};

const host = {
  alerts: [],
  files: [],
  alert(view) {
    this.alerts.push(view);
    State.view = view;
    State.mode = "expanded";
    State.notify();
  },
  collapse() {
    State.mode = "compact";
    State.notify();
  },
  takeKeyboard() {},
  setPinned(on) {
    State.isPinned = on;
  },
  chatWithFile(file) {
    this.files.push(file);
    State.view = "prompt";
    State.notify();
  },
};

const GRANT = { actionId: "act-70", digest: "cd".repeat(32), expiresAtMs: 9e12 };
const preview = (sessionId = "reg-1") => ({ sessionId, width: 400, height: 200, image: "data:image/png;base64,AA==", grant: GRANT });
const flush = () => new Promise((r) => setTimeout(r, 0));

let registered = false;
beforeEach(() => {
  resetRegionForTests();
  if (!registered) {
    registerRegion(host);
    registered = true;
  } else {
    const subscribe = State.subscribe;
    State.subscribe = () => () => {};
    registerRegion(host);
    State.subscribe = subscribe;
  }
  calls.length = 0;
  answers.clear();
  host.alerts = [];
  host.files = [];
  State.pendingApproval = null;
  State.isPinned = false;
  State.view = "prompt";
  State.mode = "expanded";
});
afterEach(resetRegionForTests);

async function picked(sessionId = "reg-1") {
  answers.set("region_start", sessionId);
  await startRegion();
  emit("region-preview", preview(sessionId));
  await flush();
}

// ── The overlay's rectangle ──────────────────────────────────────────────────

test("a drag in any direction is the same rectangle", () => {
  assert.deepEqual(between({ x: 50, y: 80 }, { x: 10, y: 20 }), { x: 10, y: 20, width: 40, height: 60 });
});

test("CSS pixels become the monitor's physical pixels, outward", () => {
  assert.deepEqual(physical({ x: 10, y: 20, width: 40, height: 60 }, 1.5), { x: 15, y: 30, width: 60, height: 90 });
  assert.deepEqual(physical({ x: 10.3, y: 0, width: 10, height: 10 }, 1.25), { x: 12, y: 0, width: 14, height: 13 });
  assert.deepEqual(physical({ x: 1, y: 1, width: 2, height: 2 }, 0), { x: 1, y: 1, width: 2, height: 2 }, "a bad ratio counts as 1");
});

test("a click is not a region", () => {
  assert.equal(isRegion({ x: 0, y: 0, width: 7, height: 400 }), false);
  assert.equal(isRegion({ x: 0, y: 0, width: 8, height: 8 }), true);
});

// ── The island's flow ────────────────────────────────────────────────────────

test("the picked picture is shown with its grant, and attached only on the click", async () => {
  await picked();
  assert.equal(Region.status, "preview");
  assert.deepEqual(host.alerts, ["region"]);
  assert.equal(State.isPinned, true, "the card keeps the island open");
  assert.equal(sent("region_attach").length, 0, "nothing is attached before the click");

  answers.set("region_attach", {
    file: { name: "Screen region.png", path: "C:/inbox/Screen region.png", size: 10 },
    receipt: { actionId: "act-70", outcome: "succeeded", evidenceRef: "inbox:png" },
  });
  assert.equal(addMask({ x: 10, y: 10, width: 50, height: 20 }), true);
  await regionAsk();
  assert.deepEqual(sent("region_attach"), [
    { sessionId: "reg-1", grant: GRANT, masks: [{ x: 10, y: 10, width: 50, height: 20 }] },
  ]);
  assert.deepEqual(host.files, [{ name: "Screen region.png", path: "C:/inbox/Screen region.png" }]);
  assert.equal(Region.status, "idle");
  assert.equal(State.isPinned, false, "the pin is given back");
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0, "never the agents' permissions");
});

test("masks are clipped to the picture, tiny ones ignored, and can be removed", async () => {
  await picked();
  assert.equal(addMask({ x: -20, y: 150, width: 100, height: 100 }), true);
  assert.deepEqual(Region.masks, [{ x: 0, y: 150, width: 80, height: 50 }]);
  assert.equal(addMask({ x: 5, y: 5, width: 1, height: 40 }), false);
  assert.equal(addMask({ x: 500, y: 5, width: 10, height: 10 }), false, "off the picture");
  removeMask(0);
  assert.deepEqual(Region.masks, []);
  for (let i = 0; i < MAX_MASKS; i++) addMask({ x: i, y: 0, width: 5, height: 5 });
  assert.equal(addMask({ x: 0, y: 0, width: 5, height: 5 }), false, "at most MAX_MASKS");
});

test("a picture from another pick is refused and dropped", async () => {
  answers.set("region_start", "reg-1");
  await startRegion();
  emit("region-preview", preview("reg-other"));
  await flush();
  assert.equal(Region.status, "picking");
  assert.equal(Region.preview, null);
  assert.deepEqual(sent("region_cancel"), [{ sessionId: "reg-other" }]);
});

test("a picture nobody asked for is dropped", async () => {
  emit("region-preview", preview("reg-1"));
  await flush();
  assert.equal(Region.preview, null);
  assert.deepEqual(host.alerts, []);
});

test("cancel drops the picture and folds the card", async () => {
  await picked();
  regionCancel();
  assert.deepEqual(sent("region_cancel"), [{ sessionId: "reg-1" }]);
  assert.equal(Region.status, "idle");
  assert.equal(State.mode, "compact");
  await regionAsk();
  assert.equal(sent("region_attach").length, 0);
});

test("leaving the card drops the picture", async () => {
  await picked();
  State.view = "overview";
  State.notify();
  assert.equal(Region.status, "idle");
  assert.deepEqual(sent("region_cancel"), [{ sessionId: "reg-1" }]);
});

test("an agent's permission card drops an unconfirmed picture", async () => {
  await picked();
  State.pendingApproval = { tool: "Bash" };
  State.notify();
  assert.equal(Region.status, "idle");
  assert.equal(sent("region_cancel").length, 1);
  State.pendingApproval = null;
});

test("a refused attach attaches nothing", async () => {
  await picked();
  answers.set("region_attach", new Error("grant-mismatch"));
  await regionAsk();
  assert.deepEqual(host.files, []);
  assert.equal(Region.status, "idle");
});

test("the overlays ending without a picture end the pick", async () => {
  answers.set("region_start", "reg-1");
  await startRegion();
  emit("region-ended", "cancelled");
  await flush();
  assert.equal(Region.status, "idle");
  assert.equal(Region.session, null);
});

test("a refused start leaves nothing behind", async () => {
  answers.set("region_start", new Error("unavailable"));
  await startRegion();
  assert.equal(Region.status, "idle");
});
