import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import ts from "typescript";
import { calls, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { IslandStateMachine } from "../src/island/fsm.ts";

// Island's upload dependency uses TypeScript parameter properties, which need
// transformation rather than Node's default type stripping. Keep that loader
// local to this test process, and exercise the production visibility methods.
registerHooks({
  load(url, context, nextLoad) {
    if (url.startsWith("file:") && url.endsWith(".ts") && !url.includes("/node_modules/")) {
      return {
        format: "module", shortCircuit: true,
        source: ts.transpileModule(readFileSync(new URL(url), "utf8"), {
          compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
        }).outputText,
      };
    }
    return nextLoad(url, context);
  },
});
const { Island } = await import("../src/island/island.ts");
const { DesktopLink } = await import("../src/island/desktop.ts");

function island() {
  // The DOM/canvas shell is irrelevant to visibility. Use its actual methods
  // and FSM without starting the rendering loop in a headless test.
  const host = Object.create(Island.prototype);
  host.collapsed = false;
  host.fsm = new IslandStateMachine();
  host.desktop = { launch() {} };
  host.expand = view => { State.mode = "expanded"; State.view = view; };
  return host;
}

beforeEach(() => {
  calls.length = 0;
  State.userHidden = false;
  State.mochiOnDesktop = false;
  State.pendingApproval = null;
  State.isPinned = false;
  State.mode = "hidden";
  State.view = "overview";
});

test("an explicit reopen reaches native visibility even after the page loses its hidden flag", () => {
  const host = island();
  // A reloaded page defaults to false while the native window can still be hidden.
  host.showIsland();
  assert.deepEqual(sent("set_island_hidden"), [{ hidden: false }]);
});

test("a reloaded hidden island skips the launch greeting and stays hidden", () => {
  const host = island();
  host.restoreVisibility(true, true);
  host.launch();
  assert.equal(host.fsm.state, "hidden");
  assert.equal(State.userHidden, true);
  assert.equal(host.collapsed, true);
  assert.deepEqual(sent("set_island_hidden"), []);
});

test("a visible first launch still starts the greeting", () => {
  const host = island();
  host.launch();
  assert.equal(host.fsm.state, "coucou");
});

test("tray, shortcut and approval alerts reopen a reloaded hidden island", () => {
  for (const view of ["overview", "prompt", "approval"]) {
    const host = island();
    State.userHidden = true;
    host.alert(view);
    assert.equal(State.userHidden, false);
    assert.equal(host.fsm.state, "home");
    assert.equal(State.view, view);
  }
  assert.deepEqual(sent("set_island_hidden"), Array(3).fill({ hidden: false }));
});

test("passive work and music reveals never reopen an explicitly hidden island", () => {
  const host = island();
  State.userHidden = true;
  host.reveal();
  host.revealSilently();
  assert.equal(host.fsm.state, "hidden");
  assert.deepEqual(sent("set_island_hidden"), []);
});

test("hiding cancels a captured move, and repeated hide waits for an explicit reopen", () => {
  const host = island();
  let cancelled = 0;
  host.pageMove = { cancel() { cancelled++; } };
  host.fsm.forceHome();
  host.hideIsland();
  host.hideIsland();
  assert.equal(host.fsm.state, "hidden");
  assert.equal(State.userHidden, true);
  assert.equal(cancelled, 1);
  host.showIsland();
  assert.equal(State.userHidden, false);
  assert.deepEqual(sent("set_island_hidden"), [{ hidden: true }, { hidden: false }]);
});


test("a reloaded wake strip expands natively when the user reopens it", () => {
  const host = island();
  host.restoreVisibility(true, true);
  host.alert("overview");
  host.updateWindowCollapsed();
  assert.deepEqual(sent("set_collapsed"), [{ collapsed: false }]);
});

test("an alert received during hidden reload is not replaced by the launch greeting", () => {
  const host = island();
  host.restoreVisibility(true, true);
  host.alert("approval");
  host.launch();
  assert.equal(host.fsm.state, "home");
  assert.equal(State.view, "approval");
});


test("skipping the greeting still restores Mochi's desktop lifecycle", async () => {
  const host = island();
  const invoke = internals.invoke;
  internals.invoke = async (command, args) => {
    const result = await invoke(command, args);
    return command === "desktop_mochi_fly_out" ? true : result;
  };
  try {
    host.desktop = new DesktopLink(host);
    host.desktop.mode = "window";
    host.desktop.controller.enabled = true;
    host.restoreVisibility(true, false);
    host.launch();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    assert.equal(host.desktop.controller.phase, "onDesktop");
    assert.equal(State.mochiOnDesktop, true);
    assert.equal(State.userHidden, true);
    assert.equal(host.fsm.state, "hidden");
    assert.deepEqual(sent("set_island_hidden"), []);
  } finally {
    internals.invoke = invoke;
  }
});
