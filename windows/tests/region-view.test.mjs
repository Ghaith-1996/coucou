// Exercise the persistent region view with the island's active-view-only sync.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";
installFakeDom();
window.addEventListener = () => {};
const { State } = await import("../src/core/state.ts");
const { Region, registerRegion, resetRegionForTests, startRegion, regionAsk, regionCancel, addMask } = await import("../src/assistant/region.ts");
const { buildRegion } = await import("../src/assistant/region-view.ts");
const answers = new Map();
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  return answers.has(cmd) ? answers.get(cmd) : result;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const host = {
  alert(view) { State.view = view; State.mode = "expanded"; State.notify(); },
  collapse() { State.mode = "compact"; State.notify(); },
  takeKeyboard() {},
  setPinned(on) { State.isPinned = on; State.notify(); },
  chatWithFile() {},
};
const view = buildRegion();
// Match the island: it registers rendering before the controller's guard.
State.subscribe(() => { if (State.view === "region") view.sync(); });
registerRegion(host);
beforeEach(() => {
  resetRegionForTests();
  const subscribe = State.subscribe;
  State.subscribe = () => () => {};
  registerRegion(host);
  State.subscribe = subscribe;
  State.pendingApproval = null;
  State.view = "prompt";
  State.mode = "expanded";
  State.isPinned = false;
  calls.length = 0;
  answers.clear();
  answers.set("region_start", "reg-1");
});
afterEach(resetRegionForTests);
const image = () => view.el.querySelector(".region-img");
const button = (label) => view.el.find("button").find((el) => el.find("span")[0]?.textContent === label);
const picture = { sessionId: "reg-1", width: 400, height: 200, image: "data:image/png;base64,AA==", grant: { actionId: "act-1", digest: "ab".repeat(32), expiresAtMs: 9e12 } };
async function picked() {
  await startRegion();
  emit("region-preview", picture);
  await flush();
  assert.equal(image().getAttribute("src"), picture.image);
}
for (const next of ["overview", "approval"]) {
  test(`leaving for ${next} clears the hidden DOM picture and masks without a view sync`, async () => {
    await picked();
    addMask({ x: 10, y: 10, width: 30, height: 30 });
    assert.equal(view.el.querySelector(".region-masks").children.length, 1);
    State.view = next;
    if (next === "approval") State.pendingApproval = { tool: "Bash" };
    State.notify();
    assert.equal(Region.status, "idle");
    assert.ok(!image().getAttribute("src"), "the hidden image must release its PNG immediately");
    assert.equal(view.el.querySelector(".region-masks").children.length, 0);
    State.pendingApproval = null;
    await picked();
    assert.equal(image().getAttribute("src"), picture.image, "even an identical later PNG must redraw");
  });
}
test("Cancel and Ask are disabled during attachment, including retained click handlers", async (t) => {
  await picked();
  const pending = Promise.withResolvers();
  const result = { file: { name: "region.png", path: "C:/inbox/region.png" } };
  t.after(async () => { pending.resolve(result); await flush(); });
  answers.set("region_attach", pending.promise);
  const attaching = regionAsk();
  assert.equal(button("Cancel").disabled, true);
  assert.equal(button("Ask about it").disabled, true);
  button("Cancel").fire("click");
  regionCancel();
  assert.equal(Region.status, "attaching");
  assert.equal(sent("region_cancel").length, 0);
  pending.resolve(result);
  await attaching;
  assert.ok(!image().getAttribute("src"));
});
