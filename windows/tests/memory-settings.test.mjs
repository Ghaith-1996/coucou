import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { installFakeDom } from "./fakedom.mjs";
import { calls, sent, internals, emit } from "./tauri.mjs";
import { onEvent } from "../src/core/bridge.ts";
import { memorySection, refreshMemorySection } from "../src/settings/memory.ts";
import { LANGUAGE_CODES, setLanguage, t as translate } from "../src/i18n/i18n.ts";

installFakeDom();

const item = (id, text, source = "user") => ({ id, text, source, createdAt: 1, updatedAt: 1 });
const state = (items, enabled = true) => ({ version: 1, enabled, items });
const settle = () => new Promise((resolve) => setImmediate(resolve));
const button = (root, label) => root.find("button").find((b) => b.textContent === label);

/** Answers the memory commands like memory.rs would, from `replies`. */
function answering(t, replies) {
  const original = internals.invoke;
  t.mock.method(internals, "invoke", async (cmd, args) => {
    if (cmd in replies) {
      calls.push([cmd, args]);
      const reply = replies[cmd];
      if (reply instanceof Error) throw reply;
      return typeof reply === "function" ? reply(args) : reply;
    }
    return original(cmd, args);
  });
}

beforeEach(() => { calls.length = 0; });

test("lists what is remembered and marks what the chat proposed", async (t) => {
  answering(t, { memory_list: state([item("m1", "Prefers French"), item("m2", "Works on Coucou", "chat")]) });
  const section = memorySection();
  await settle();
  const rows = section.find(".memory-item");
  assert.equal(rows.length, 2);
  assert.equal(rows[0].find(".memory-badge").length, 0);
  assert.equal(rows[1].find(".memory-badge")[0].textContent, "From the chat");
  assert.match(section.textContent, /Prefers French/);
  assert.equal(section.find(".switch")[0].getAttribute("aria-pressed"), "true");
});

test("an empty memory says so and hides the delete-everything button", async (t) => {
  answering(t, { memory_list: state([]) });
  const section = memorySection();
  await settle();
  assert.match(section.textContent, /Nothing yet\./);
  assert.equal(button(section, "Delete everything").hidden, true);
});

test("adding sends the trimmed text and draws what Rust returns", async (t) => {
  answering(t, {
    memory_list: state([]),
    memory_add: ({ text }) => state([item("m1", text)]),
  });
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  input.value = "  Likes short answers  ";
  button(section, "Add").fire("click");
  await settle();
  assert.deepEqual(sent("memory_add"), [{ text: "Likes short answers" }]);
  assert.equal(input.value, "");
  assert.match(section.textContent, /Likes short answers/);
});

test("blank text is never sent", async (t) => {
  answering(t, { memory_list: state([]) });
  const section = memorySection();
  await settle();
  section.find("input")[0].value = "   ";
  button(section, "Add").fire("click");
  await settle();
  assert.equal(sent("memory_add").length, 0);
});

test("a refusal from Rust is shown, not swallowed", async (t) => {
  answering(t, {
    memory_list: state([]),
    memory_add: new Error("Memory is full: delete something first."),
  });
  const section = memorySection();
  await settle();
  section.find("input")[0].value = "One more";
  button(section, "Add").fire("click");
  await settle();
  assert.match(section.textContent, /Memory is full: delete something first\./);
});

test("editing an item sends its id and the new text", async (t) => {
  answering(t, {
    memory_list: state([item("m1", "Old")]),
    memory_edit: ({ id, text }) => state([item(id, text)]),
  });
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = section.find(".memory-item")[0].find("input")[0];
  field.value = "New";
  button(section, "Save").fire("click");
  await settle();
  assert.deepEqual(sent("memory_edit"), [{ id: "m1", text: "New" }]);
  assert.match(section.textContent, /New/);
  assert.equal(section.find(".memory-item")[0].find("input").length, 0, "back to reading");
});

test("deleting one item names it", async (t) => {
  answering(t, {
    memory_list: state([item("m1", "One"), item("m2", "Two")]),
    memory_delete: () => state([item("m2", "Two")]),
  });
  const section = memorySection();
  await settle();
  section.find(".memory-item")[0].find("button").find((b) => b.textContent === "Delete").fire("click");
  await settle();
  assert.deepEqual(sent("memory_delete"), [{ id: "m1" }]);
  assert.doesNotMatch(section.textContent, /One/);
});

test("deleting everything takes a second click", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  answering(t, {
    memory_list: state([item("m1", "One")]),
    memory_clear: state([]),
  });
  const section = memorySection();
  await settle();
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  await settle();
  assert.equal(sent("memory_clear").length, 0, "the first click only asks");
  assert.equal(clearBtn.textContent, "Really delete everything?");
  clearBtn.fire("click");
  await settle();
  assert.equal(sent("memory_clear").length, 1);
  assert.match(section.textContent, /Nothing yet\./);
});

test("the delete-everything question goes away after 4 s", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  answering(t, { memory_list: state([item("m1", "One")]), memory_clear: state([]) });
  const section = memorySection();
  await settle();
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  t.mock.timers.tick(4000);
  assert.equal(clearBtn.textContent, "Delete everything");
  clearBtn.fire("click");
  await settle();
  assert.equal(sent("memory_clear").length, 0, "asks again instead of deleting");
});

test("the switch turns memory off without deleting anything", async (t) => {
  answering(t, {
    memory_list: state([item("m1", "One")]),
    memory_set_enabled: ({ enabled }) => state([item("m1", "One")], enabled),
  });
  const section = memorySection();
  await settle();
  section.find(".switch")[0].fire("click");
  await settle();
  assert.deepEqual(sent("memory_set_enabled"), [{ enabled: false }]);
  assert.equal(section.find(".switch")[0].getAttribute("aria-pressed"), "false");
  assert.match(section.textContent, /One/);
  assert.equal(sent("memory_delete").length + sent("memory_clear").length, 0);
});

test("export says where the file went", async (t) => {
  answering(t, {
    memory_list: state([item("m1", "One")]),
    memory_export: "C:\\Users\\me\\Downloads\\coucou-memory.json",
  });
  const section = memorySection();
  await settle();
  button(section, "Export unencrypted JSON").fire("click");
  await settle();
  assert.match(section.textContent, /Saved to C:\\Users\\me\\Downloads\\coucou-memory\.json/);
});

test("the section says memory is sent to the chat's model", async (t) => {
  answering(t, { memory_list: state([]) });
  const section = memorySection();
  await settle();
  assert.match(section.textContent, /goes with every question to the model you chat with/);
});


test("a duplicate edit refusal keeps both rows and the draft, then permits a corrected retry", async (t) => {
  const first = item("m1", "I use Rust", "chat");
  const other = item("m2", "I write in French");
  let edits = 0;
  answering(t, {
    memory_list: state([first, other]),
    memory_edit: ({ id, text }) => {
      if (++edits === 1) throw new Error("This memory already exists.");
      return state([{ ...first, id, text, updatedAt: 9 }, other]);
    },
  });
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = section.find(".memory-item")[0].find("input")[0];
  field.value = "i WRITE in FRENCH";
  button(section, "Save").fire("click");
  await settle();
  assert.match(section.textContent, /This memory already exists\./);
  assert.equal(section.find(".memory-item").length, 2);
  assert.equal(section.find(".memory-item")[0].find("input")[0], field, "keep the editable draft after refusal");
  assert.equal(field.value, "i WRITE in FRENCH");
  assert.match(section.find(".memory-item")[1].textContent, /I write in French/);
  assert.equal(sent("memory_add").length + sent("memory_delete").length, 0);
  field.value = "I use Rust at work";
  button(section, "Save").fire("click");
  await settle();
  assert.deepEqual(sent("memory_edit"), [
    { id: "m1", text: "i WRITE in FRENCH" },
    { id: "m1", text: "I use Rust at work" },
  ]);
  const rows = section.find(".memory-item");
  assert.equal(rows.length, 2);
  assert.equal(rows[0].find("input").length, 0);
  assert.match(rows[0].textContent, /I use Rust at work/);
  assert.equal(rows[0].find(".memory-badge")[0].textContent, "From the chat");
  assert.match(rows[1].textContent, /I write in French/);
  assert.doesNotMatch(section.textContent, /This memory already exists\./);
});


test("a memory read failure is visible and a failed export never claims a backup was saved", async (t) => {
  answering(t, {
    memory_list: new Error("memory.bin could not be read: sharing violation"),
    memory_export: new Error("memory.bin could not be read: sharing violation"),
  });
  const section = memorySection();
  await settle();
  assert.match(section.textContent, /memory.bin could not be read/);
  button(section, "Export unencrypted JSON").fire("click");
  await settle();
  assert.match(section.textContent, /sharing violation/);
  assert.doesNotMatch(section.textContent, /Saved to/);
  assert.match(section.textContent, /Export saves an unencrypted JSON file in Downloads/);
  assert.match(section.textContent, /Anyone with access to that file can read your memories/);
});

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

test("the memory switch waits for authoritative initial state instead of toggling its default", async (t) => {
  const initial = deferred();
  answering(t, {
    memory_list: initial.promise,
    memory_set_enabled: ({ enabled }) => state([], enabled),
  });
  const section = memorySection();
  const toggle = section.find(".switch")[0];
  toggle.fire("click");
  assert.deepEqual(sent("memory_set_enabled"), []);
  assert.equal(toggle.disabled, true);
  initial.resolve(state([], false));
  await settle();
  assert.equal(toggle.disabled, false);
  assert.equal(toggle.getAttribute("aria-pressed"), "false");
  toggle.fire("click");
  await settle();
  assert.deepEqual(sent("memory_set_enabled"), [{ enabled: true }]);
});

test("pending memory toggle disables duplicate clicks and the next click uses its confirmed state", async (t) => {
  const first = deferred();
  let toggles = 0;
  answering(t, {
    memory_list: state([]),
    memory_set_enabled: ({ enabled }) => ++toggles === 1 ? first.promise : state([], enabled),
  });
  const section = memorySection();
  await settle();
  const toggle = section.find(".switch")[0];
  toggle.fire("click");
  toggle.fire("click");
  assert.deepEqual(sent("memory_set_enabled"), [{ enabled: false }]);
  assert.equal(toggle.disabled, true);
  first.resolve(state([], false));
  await settle();
  assert.equal(toggle.disabled, false);
  assert.equal(toggle.getAttribute("aria-pressed"), "false");
  toggle.fire("click");
  await settle();
  assert.deepEqual(sent("memory_set_enabled"), [{ enabled: false }, { enabled: true }]);
  assert.equal(toggle.getAttribute("aria-pressed"), "true");
});

test("a failed toggle keeps confirmed state and permits an explicit retry", async (t) => {
  const first = deferred();
  let toggles = 0;
  answering(t, {
    memory_list: state([]),
    memory_set_enabled: ({ enabled }) => ++toggles === 1 ? first.promise : state([], enabled),
  });
  const section = memorySection();
  await settle();
  const toggle = section.find(".switch")[0];
  toggle.fire("click");
  assert.equal(toggle.disabled, true);
  first.reject(new Error("storage unavailable"));
  await settle();
  assert.equal(toggle.disabled, false);
  assert.equal(toggle.getAttribute("aria-pressed"), "true");
  assert.match(section.textContent, /storage unavailable/);
  toggle.fire("click");
  await settle();
  assert.deepEqual(sent("memory_set_enabled"), [{ enabled: false }, { enabled: false }]);
  assert.equal(toggle.getAttribute("aria-pressed"), "false");
  assert.doesNotMatch(section.textContent, /storage unavailable/);
});

test("an initial read failure never enables a switch based on guessed state", async (t) => {
  answering(t, { memory_list: new Error("unreadable memory") });
  const section = memorySection();
  await settle();
  const toggle = section.find(".switch")[0];
  toggle.fire("click");
  assert.deepEqual(sent("memory_set_enabled"), []);
  assert.equal(toggle.disabled, true);
  assert.match(section.textContent, /unreadable memory/);
});


test("export discloses the unencrypted file and destination before any click or native reply", async (t) => {
  const initial = deferred();
  answering(t, { memory_list: initial.promise, memory_export: "Downloads/memory.json" });
  const section = memorySection();
  const label = "Export unencrypted JSON";
  const warning = "Export saves an unencrypted JSON file in Downloads. Anyone with access to that file can read your memories.";
  assert.ok(button(section, label), "the export action itself names the unencrypted format");
  assert.match(section.textContent, /Export saves an unencrypted JSON file in Downloads/);
  assert.match(section.textContent, /Anyone with access to that file can read your memories/);
  assert.ok(section.textContent.indexOf(warning) < section.textContent.indexOf(label), "the warning appears before the action");
  assert.deepEqual(sent("memory_export"), [], "rendering a disclosure never creates a file");
  initial.resolve(state([]));
  await settle();
  button(section, label).fire("click");
  await settle();
  assert.equal(sent("memory_export").length, 1, "an explicit export click still works");
  assert.match(section.textContent, /Saved to Downloads\/memory.json/);
  assert.ok(section.textContent.includes(warning), "the disclosure remains after exporting");
});

test("the plaintext export label and warning are visible in every supported language", async (t) => {
  answering(t, { memory_list: state([]) });
  const label = "Export unencrypted JSON";
  const warning = "Export saves an unencrypted JSON file in Downloads. Anyone with access to that file can read your memories.";
  try {
    for (const lang of LANGUAGE_CODES) {
      setLanguage(lang);
      const section = memorySection();
      assert.ok(button(section, translate(label)), `${lang}: export label`);
      assert.ok(section.textContent.includes(translate(warning)), `${lang}: visible warning`);
      if (lang !== "en") {
        assert.notEqual(translate(label), label, `${lang}: translated label`);
        assert.notEqual(translate(warning), warning, `${lang}: translated warning`);
      }
    }
    assert.deepEqual(sent("memory_export"), []);
    await settle();
  } finally {
    setLanguage("en");
  }
});


test("adding waits for initial memory state so a stale list cannot hide a new fact", async (t) => {
  const initial = deferred();
  const old = item("old", "Existing fact");
  answering(t, {
    memory_list: initial.promise,
    memory_add: ({ text }) => state([old, item("new", text)]),
  });
  const section = memorySection();
  const field = section.find("input")[0];
  const addBtn = button(section, "Add");
  field.value = "New fact";
  addBtn.fire("click");
  for (const listener of field.listeners.get("keydown") ?? []) listener({ key: "Enter" });
  await settle();
  assert.deepEqual(sent("memory_add"), [], "pending initial state blocks both click and keyboard mutations");
  assert.equal(field.disabled, true);
  assert.equal(addBtn.disabled, true);
  initial.resolve(state([old]));
  await settle();
  assert.equal(field.disabled, false);
  assert.equal(addBtn.disabled, false);
  assert.equal(field.value, "New fact", "loading never erases a draft");
  addBtn.fire("click");
  await settle();
  assert.deepEqual(sent("memory_add"), [{ text: "New fact" }]);
  assert.match(section.textContent, /Existing fact/);
  assert.match(section.textContent, /New fact/);
  assert.equal(section.find(".memory-item").length, 2);
});

test("a failed initial memory load keeps Add guarded and preserves the error and draft", async (t) => {
  answering(t, {
    memory_list: new Error("memory could not be loaded"),
    memory_add: state([item("unexpected", "New fact")]),
  });
  const section = memorySection();
  await settle();
  const field = section.find("input")[0];
  const addBtn = button(section, "Add");
  field.value = "New fact";
  addBtn.fire("click");
  for (const listener of field.listeners.get("keydown") ?? []) listener({ key: "Enter" });
  await settle();
  assert.deepEqual(sent("memory_add"), []);
  assert.equal(field.disabled, true);
  assert.equal(addBtn.disabled, true);
  assert.equal(field.value, "New fact");
  assert.match(section.textContent, /memory could not be loaded/);
});


test("an add preserves a newer draft and reserves only submission until it completes", async (t) => {
  const pending = deferred();
  let adds = 0;
  answering(t, {
    memory_list: state([]),
    memory_add: ({ text }) => ++adds === 1 ? pending.promise : state([item("one", "First fact"), item("two", text)]),
  });
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  const addBtn = button(section, "Add");
  input.value = "  First fact  ";
  addBtn.fire("click");
  input.value = "Second fact";
  addBtn.fire("click");
  for (const listener of input.listeners.get("keydown") ?? []) listener({ key: "Enter" });
  assert.deepEqual(sent("memory_add"), [{ text: "First fact" }], "duplicate and next submissions wait for the active write");
  assert.equal(input.disabled, false, "typing the next draft remains possible");
  assert.equal(addBtn.disabled, true);
  pending.resolve(state([item("one", "First fact")]));
  await settle();
  assert.equal(input.value, "Second fact", "the completed request owns only its submitted draft");
  assert.equal(addBtn.disabled, false);
  assert.match(section.textContent, /First fact/);
  addBtn.fire("click");
  await settle();
  assert.deepEqual(sent("memory_add"), [{ text: "First fact" }, { text: "Second fact" }]);
  assert.equal(input.value, "");
  assert.equal(section.find(".memory-item").length, 2);
});

test("a failed pending add preserves a replacement draft and releases submission for retry", async (t) => {
  const pending = deferred();
  let adds = 0;
  answering(t, {
    memory_list: state([]),
    memory_add: ({ text }) => ++adds === 1 ? pending.promise : state([item("one", text)]),
  });
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  const addBtn = button(section, "Add");
  input.value = "First fact";
  addBtn.fire("click");
  input.value = "Corrected fact";
  pending.reject(new Error("storage temporarily unavailable"));
  await settle();
  assert.equal(input.value, "Corrected fact");
  assert.equal(addBtn.disabled, false);
  assert.match(section.textContent, /storage temporarily unavailable/);
  assert.equal(section.find(".memory-item").length, 0);
  for (const listener of input.listeners.get("keydown") ?? []) listener({ key: "Enter" });
  await settle();
  assert.deepEqual(sent("memory_add"), [{ text: "First fact" }, { text: "Corrected fact" }]);
  assert.equal(input.value, "");
  assert.doesNotMatch(section.textContent, /storage temporarily unavailable/);
  assert.match(section.textContent, /Corrected fact/);
});

test("an add clears its own unchanged raw draft after successful persistence", async (t) => {
  const pending = deferred();
  answering(t, { memory_list: state([]), memory_add: pending.promise });
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  const addBtn = button(section, "Add");
  input.value = "  First fact  ";
  addBtn.fire("click");
  pending.resolve(state([item("one", "First fact")]));
  await settle();
  assert.equal(input.value, "");
  assert.equal(addBtn.disabled, false);
  assert.deepEqual(sent("memory_add"), [{ text: "First fact" }]);
});


test("completion of one add never clears text typed for the next memory", async (t) => {
  const pending = deferred();
  answering(t, { memory_list: state([]), memory_add: pending.promise });
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  input.value = "First fact";
  button(section, "Add").fire("click");
  input.value = "Next draft";
  pending.resolve(state([item("one", "First fact")]));
  await settle();
  assert.equal(input.value, "Next draft");
  assert.deepEqual(sent("memory_add"), [{ text: "First fact" }]);
});

const key = (field, key) => {
  for (const listener of field.listeners.get("keydown") ?? []) listener({ key });
};
const editField = section => section.find(".memory-item").flatMap(row => row.find("input"))[0];

function pendingEdit(t, extra = {}) {
  const pending = deferred();
  const original = [item("one", "First fact"), item("two", "Second fact")];
  let edits = 0;
  answering(t, {
    memory_list: state(original),
    memory_edit: ({ id, text }) => ++edits === 1 ? pending.promise : state(original.map(row => row.id === id ? { ...row, text } : row)),
    ...extra,
  });
  return { pending, original };
}

test("edit completion preserves a correction typed after Save, then saves it on an explicit retry", async t => {
  const { pending, original } = pendingEdit(t);
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Submitted fact";
  button(section, "Save").fire("click");
  field.value = "Newer correction";
  pending.resolve(state([{ ...original[0], text: "Submitted fact" }, original[1]]));
  await settle();
  assert.equal(editField(section), field, "the newer draft keeps its editor and input");
  assert.equal(field.value, "Newer correction");
  assert.equal(button(section, "Save").disabled, false);
  key(field, "Enter");
  await settle();
  assert.deepEqual(sent("memory_edit"), [{ id: "one", text: "Submitted fact" }, { id: "one", text: "Newer correction" }]);
  assert.equal(editField(section), undefined, "an unchanged successfully saved draft closes");
  assert.match(section.textContent, /Newer correction/);
});

test("pending edit reserves Save and Enter while leaving the draft editable", async t => {
  const { pending, original } = pendingEdit(t);
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  const save = button(section, "Save");
  field.value = "  Submitted fact  ";
  save.fire("click");
  field.value = "Next correction";
  save.fire("click");
  key(field, "Enter");
  assert.deepEqual(sent("memory_edit"), [{ id: "one", text: "  Submitted fact  " }]);
  assert.equal(save.disabled, true);
  assert.notEqual(field.disabled, true);
  pending.resolve(state([{ ...original[0], text: "Submitted fact" }, original[1]]));
  await settle();
  assert.equal(editField(section), field);
  assert.equal(field.value, "Next correction");
  assert.equal(save.disabled, false);
});

test("an edit failure preserves the latest draft and releases Save for retry", async t => {
  const { pending } = pendingEdit(t);
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Submitted fact";
  button(section, "Save").fire("click");
  field.value = "Correction after submit";
  pending.reject(new Error("disk unavailable"));
  await settle();
  assert.equal(editField(section), field);
  assert.equal(field.value, "Correction after submit");
  assert.equal(button(section, "Save").disabled, false);
  assert.match(section.textContent, /disk unavailable/);
  button(section, "Save").fire("click");
  await settle();
  assert.equal(editField(section), undefined);
  assert.match(section.textContent, /Correction after submit/);
  assert.doesNotMatch(section.textContent, /disk unavailable/);
});

for (const outcome of ["success", "failure"]) {
  for (const replacement of ["same item", "another item"]) {
    test(`a canceled edit's late ${outcome} preserves the replacement editor for ${replacement}`, async t => {
      const { pending, original } = pendingEdit(t, { memory_export: new Error("newer export error") });
      const section = memorySection();
      await settle();
      button(section, "Edit").fire("click");
      const oldField = editField(section);
      const oldSave = button(section, "Save");
      const oldCancel = button(section, "Cancel");
      oldField.value = "Old submitted value";
      oldSave.fire("click");
      if (replacement === "same item") key(oldField, "Escape");
      else oldCancel.fire("click");
      const index = replacement === "same item" ? 0 : 1;
      button(section.find(".memory-item")[index], "Edit").fire("click");
      const replacementField = editField(section);
      replacementField.value = "Replacement draft";
      button(section, "Save").fire("click");
      oldSave.fire("click");
      oldCancel.fire("click");
      key(oldField, "Escape");
      key(oldField, "Enter");
      assert.equal(sent("memory_edit").length, 1, "a retired editor cannot submit or overlap the pending edit");
      assert.equal(editField(section), replacementField, "retired controls cannot cancel a replacement editor");
      assert.equal(button(section, "Save").disabled, true);
      button(section, "Export unencrypted JSON").fire("click");
      await settle();
      if (outcome === "success") pending.resolve(state([{ ...original[0], text: "Old submitted value" }, original[1]]));
      else pending.reject(new Error("old edit failure"));
      await settle();
      assert.equal(editField(section), replacementField);
      assert.equal(replacementField.value, "Replacement draft");
      assert.equal(button(section, "Save").disabled, false);
      assert.match(section.textContent, /newer export error/, "an old editor does not replace a newer operation's message");
      assert.doesNotMatch(section.textContent, /old edit failure/);
      key(replacementField, "Enter");
      await settle();
      assert.deepEqual(sent("memory_edit"), [
        { id: "one", text: "Old submitted value" },
        { id: index ? "two" : "one", text: "Replacement draft" },
      ]);
      assert.equal(editField(section), undefined);
    });
  }
}

for (const saving of [false, true]) {
  test(`a ${saving ? "pending" : "not yet saved"} edit retains its exact draft through unrelated redraws`, async t => {
    const { pending, original } = pendingEdit(t, { memory_set_enabled: ({ enabled }) => state(original, enabled) });
    const section = memorySection();
    await settle();
    button(section, "Edit").fire("click");
    const field = editField(section);
    field.value = "First edit";
    if (saving) button(section, "Save").fire("click");
    field.value = "  Unsaved correction  ";
    section.find(".switch")[0].fire("click");
    assert.equal(editField(section), field, "the immediate toggle redraw reuses the active field");
    await settle();
    assert.equal(editField(section), field);
    assert.equal(field.value, "  Unsaved correction  ");
    assert.equal(button(section, "Save").disabled, saving);
    if (saving) {
      pending.resolve(state([{ ...original[0], text: "First edit" }, original[1]], false));
      await settle();
      assert.equal(editField(section), field);
      assert.equal(field.value, "  Unsaved correction  ");
      assert.equal(button(section, "Save").disabled, false);
    }
  });
}

test("cancel without replacement does not reopen an editor when its save completes", async t => {
  const { pending, original } = pendingEdit(t);
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Submitted fact";
  button(section, "Save").fire("click");
  button(section, "Cancel").fire("click");
  pending.resolve(state([{ ...original[0], text: "Submitted fact" }, original[1]]));
  await settle();
  assert.equal(editField(section), undefined);
  assert.match(section.textContent, /Submitted fact/, "Cancel abandons editing, not a write already submitted");
});

test("an editor redraw restores the focused field and its selection", async t => {
  const original = [item("one", "First fact")];
  answering(t, { memory_list: state(original), memory_set_enabled: ({ enabled }) => state(original, enabled) });
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Unsaved draft";
  field.selectionStart = 2;
  field.selectionEnd = 8;
  document.activeElement = field;
  t.after(() => { delete document.activeElement; });
  let focusCalls = 0;
  const ranges = [];
  field.focus = () => { focusCalls++; };
  field.setSelectionRange = (start, end) => { ranges.push([start, end]); };
  section.find(".switch")[0].fire("click");
  await settle();
  assert.equal(editField(section), field);
  assert.equal(field.value, "Unsaved draft");
  assert.ok(focusCalls > 0);
  assert.ok(ranges.length > 0);
  assert.ok(ranges.every(range => range[0] === 2 && range[1] === 8));
});

test("switching rows during Save gives ownership and delayed focus only to the replacement editor", async t => {
  const frames = [];
  t.mock.method(globalThis, "requestAnimationFrame", frame => { frames.push(frame); return frames.length; });
  const { pending, original } = pendingEdit(t);
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const first = editField(section);
  let firstFocus = 0;
  first.focus = () => { firstFocus++; };
  first.value = "Submitted fact";
  button(section, "Save").fire("click");
  button(section.find(".memory-item")[1], "Edit").fire("click");
  const second = editField(section);
  let secondFocus = 0;
  second.focus = () => { secondFocus++; };
  second.value = "Replacement draft";
  frames.forEach(frame => frame());
  assert.equal(firstFocus, 0, "the previous frame cannot reclaim focus");
  assert.equal(secondFocus, 1);
  assert.equal(button(section, "Save").disabled, true);
  pending.resolve(state([{ ...original[0], text: "Submitted fact" }, original[1]]));
  await settle();
  assert.equal(editField(section), second);
  assert.equal(second.value, "Replacement draft");
  assert.equal(button(section, "Save").disabled, false);
});

test("deleting all memory retires the editor and its retained submit handlers", async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  answering(t, { memory_list: state([item("one", "First fact")]), memory_clear: state([]) });
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Abandoned draft";
  const save = button(section, "Save");
  const clear = button(section, "Delete everything");
  clear.fire("click");
  assert.equal(editField(section), field, "confirmation alone keeps the draft");
  clear.fire("click");
  await settle();
  assert.equal(editField(section), undefined);
  save.fire("click");
  key(field, "Enter");
  assert.deepEqual(sent("memory_edit"), []);
  assert.equal(section.find(".memory-item").length, 0);
});


test("pending clear cannot re-arm deletion for a newly added item", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = deferred();
  answering(t, {
    memory_list: state([item("old", "Old fact")]),
    memory_clear: pending.promise,
    memory_add: ({ text }) => state([item("new", text)]),
  });
  const section = memorySection();
  await settle();
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  clearBtn.fire("click");
  clearBtn.fire("click");
  assert.equal(sent("memory_clear").length, 1);
  assert.equal(clearBtn.disabled, true);
  assert.equal(clearBtn.textContent, "Delete everything", "pending deletion is no longer armed");
  pending.resolve(state([]));
  await settle();
  assert.equal(clearBtn.hidden, true);
  section.find("input")[0].value = "New fact";
  button(section, "Add").fire("click");
  await settle();
  assert.equal(clearBtn.disabled, false);
  assert.equal(clearBtn.textContent, "Delete everything");
  clearBtn.fire("click");
  await settle();
  assert.equal(sent("memory_clear").length, 1, "the new item requires a fresh first confirmation click");
  assert.equal(clearBtn.textContent, "Really delete everything?");
});

test("failed clear releases controls but requires fresh confirmation for retry", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = deferred();
  let clears = 0;
  answering(t, {
    memory_list: state([item("old", "Old fact")]),
    memory_clear: () => ++clears === 1 ? pending.promise : state([]),
  });
  const section = memorySection();
  await settle();
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  clearBtn.fire("click");
  clearBtn.fire("click");
  pending.reject(new Error("clear failed"));
  await settle();
  assert.equal(clearBtn.disabled, false);
  assert.equal(clearBtn.textContent, "Delete everything");
  assert.match(section.textContent, /Old fact/);
  assert.match(section.textContent, /clear failed/);
  clearBtn.fire("click");
  assert.equal(sent("memory_clear").length, 1);
  clearBtn.fire("click");
  await settle();
  assert.equal(sent("memory_clear").length, 2);
  assert.match(section.textContent, /Nothing yet/);
  assert.doesNotMatch(section.textContent, /clear failed/);
});

test("new memory state invalidates an old clear confirmation and its timer", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  answering(t, {
    memory_list: state([item("old", "Old fact")]),
    memory_add: ({ text }) => state([item("old", "Old fact"), item("new", text)]),
    memory_clear: state([]),
  });
  const section = memorySection();
  await settle();
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  t.mock.timers.tick(3000);
  section.find("input")[0].value = "New fact";
  button(section, "Add").fire("click");
  await settle();
  assert.equal(clearBtn.textContent, "Delete everything", "updated state invalidates the previous confirmation");
  clearBtn.fire("click");
  assert.equal(sent("memory_clear").length, 0);
  t.mock.timers.tick(1000);
  assert.equal(clearBtn.textContent, "Really delete everything?", "the old timer cannot expire the newer confirmation");
  t.mock.timers.tick(3000);
  assert.equal(clearBtn.textContent, "Delete everything");
});

test("hidden clear controls cannot arm or delete an empty or unloaded store", async (t) => {
  const pending = deferred();
  answering(t, { memory_list: pending.promise, memory_clear: state([]) });
  const section = memorySection();
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  clearBtn.fire("click");
  assert.equal(sent("memory_clear").length, 0);
  pending.resolve(state([]));
  await settle();
  clearBtn.fire("click");
  clearBtn.fire("click");
  assert.equal(sent("memory_clear").length, 0);
});

// Execute the production focus registration against an EventTarget. The page
// itself boots unrelated Settings sections, which these Memory DOM tests omit.
function settingsFocus() {
  const source = readFileSync(new URL("../src/settings/main.ts", import.meta.url), "utf8");
  const registrations = source.match(/window\.addEventListener\("focus", refreshMemorySection\);/g) ?? [];
  assert.equal(registrations.length, 1, "one page-level listener, never one per Memory section");
  assert.ok(source.indexOf(registrations[0]) < source.indexOf("async function main()"), "the listener is outside render/boot loops");
  const target = new EventTarget();
  new Function("window", "refreshMemorySection", registrations[0])(target, refreshMemorySection);
  return () => target.dispatchEvent(new Event("focus"));
}

test("Settings focus reveals chat-confirmed memory and its badge without replacing either draft", async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const original = item("one", "First fact");
  let stored = state([original]);
  answering(t, { memory_list: () => stored });
  const focus = settingsFocus();
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  input.value = "  Next Add draft  ";
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "  Unsaved correction  ";
  const clearBtn = button(section, "Delete everything");
  clearBtn.fire("click");
  stored = state([original, item("external", "Confirmed in chat", "chat")]);
  focus();
  await settle();
  const rows = section.find(".memory-item");
  assert.equal(rows.length, 2);
  assert.match(rows[1].textContent, /Confirmed in chat/);
  assert.equal(rows[1].find(".memory-badge")[0].textContent, "From the chat");
  assert.equal(section.find("input")[0], input);
  assert.equal(input.value, "  Next Add draft  ");
  assert.equal(editField(section), field);
  assert.equal(field.value, "  Unsaved correction  ");
  assert.equal(clearBtn.textContent, "Delete everything", "a new authoritative snapshot needs a fresh confirmation");
});

test("overlapping focus refreshes are coalesced and an older snapshot is never applied", async t => {
  const oldRead = deferred();
  const freshRead = deferred();
  let reads = 0;
  answering(t, { memory_list: () => [state([item("one", "Original")]), oldRead.promise, freshRead.promise][reads++] });
  const section = memorySection();
  await settle();
  refreshMemorySection();
  refreshMemorySection();
  refreshMemorySection();
  assert.equal(sent("memory_list").length, 2, "only one refresh runs at a time");
  oldRead.resolve(state([item("stale", "Stale response")]));
  await settle();
  assert.equal(sent("memory_list").length, 3, "multiple focus events queue just one newer read");
  assert.match(section.textContent, /Original/);
  assert.doesNotMatch(section.textContent, /Stale response/);
  freshRead.resolve(state([item("fresh", "Newest chat fact", "chat")]));
  await settle();
  assert.match(section.textContent, /Newest chat fact/);
  assert.doesNotMatch(section.textContent, /Original/);
});

test("a focus queued during initial loading waits for the newer snapshot before enabling mutations", async t => {
  const initial = deferred();
  const fresh = deferred();
  let reads = 0;
  answering(t, { memory_list: () => ++reads === 1 ? initial.promise : fresh.promise });
  const section = memorySection();
  refreshMemorySection();
  initial.resolve(state([]));
  await settle();
  assert.equal(sent("memory_list").length, 2);
  assert.equal(button(section, "Add").disabled, true);
  fresh.resolve(state([item("chat", "Confirmed during load", "chat")]));
  await settle();
  assert.equal(button(section, "Add").disabled, false);
  assert.match(section.textContent, /Confirmed during load/);
});

const mutations = [
  { name: "Add", command: "memory_add", run(section) {
    section.find("input")[0].value = "Locally added";
    button(section, "Add").fire("click");
  }, result: state([item("one", "Original"), item("local", "Locally added")]) },
  { name: "Edit", command: "memory_edit", run(section) {
    button(section, "Edit").fire("click");
    editField(section).value = "Locally edited";
    button(section, "Save").fire("click");
  }, result: state([item("one", "Locally edited")]) },
  { name: "Delete", command: "memory_delete", run(section) { button(section, "Delete").fire("click"); }, result: state([]) },
  { name: "toggle", command: "memory_set_enabled", run(section) { section.find(".switch")[0].fire("click"); }, result: state([item("one", "Original")], false) },
  { name: "clear", command: "memory_clear", run(section) {
    const clearBtn = button(section, "Delete everything");
    clearBtn.fire("click");
    clearBtn.fire("click");
  }, result: state([]) },
];

for (const mutation of mutations) {
  test(`a late refresh cannot undo a completed ${mutation.name} and rereads after the write`, async t => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const oldRead = deferred();
    const freshRead = deferred();
    let reads = 0;
    answering(t, {
      memory_list: () => [state([item("one", "Original")]), oldRead.promise, freshRead.promise][reads++],
      [mutation.command]: mutation.result,
    });
    const section = memorySection();
    await settle();
    refreshMemorySection();
    mutation.run(section);
    await settle();
    const afterMutation = section.textContent;
    const enabled = section.find(".switch")[0].getAttribute("aria-pressed");
    oldRead.resolve(state([item("stale", "Stale response")]));
    await settle();
    assert.equal(section.textContent, afterMutation, "an older read must not replace the successful write");
    assert.equal(section.find(".switch")[0].getAttribute("aria-pressed"), enabled);
    assert.equal(sent("memory_list").length, 3, "invalidate and reconcile the overlapping read");
    freshRead.resolve(state([...mutation.result.items, item("chat", "New chat fact", "chat")], mutation.result.enabled));
    await settle();
    assert.match(section.textContent, /New chat fact/);
  });

  test(`focus during pending ${mutation.name} defers its read until the mutation settles`, async t => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const pending = deferred();
    let stored = state([item("one", "Original")]);
    answering(t, { memory_list: () => stored, [mutation.command]: pending.promise });
    const section = memorySection();
    await settle();
    mutation.run(section);
    refreshMemorySection();
    refreshMemorySection();
    assert.equal(sent("memory_list").length, 1, "do not read an intermediate native snapshot");
    stored = state([...mutation.result.items, item("chat", "New chat fact", "chat")], mutation.result.enabled);
    pending.resolve(mutation.result);
    await settle();
    assert.equal(sent("memory_list").length, 2);
    assert.match(section.textContent, /New chat fact/);
  });
}

test("refresh failures and later successful reads preserve a newer local mutation error and draft", async t => {
  const oldRead = deferred();
  const freshRead = deferred();
  let reads = 0;
  answering(t, {
    memory_list: () => [state([]), oldRead.promise, freshRead.promise][reads++],
    memory_add: new Error("local write failed"),
  });
  const section = memorySection();
  await settle();
  refreshMemorySection();
  const input = section.find("input")[0];
  input.value = "Retry this draft";
  button(section, "Add").fire("click");
  await settle();
  oldRead.reject(new Error("outdated read failed"));
  await settle();
  assert.match(section.textContent, /local write failed/);
  assert.doesNotMatch(section.textContent, /outdated read failed/);
  freshRead.resolve(state([item("chat", "External fact", "chat")]));
  await settle();
  assert.match(section.textContent, /local write failed/, "a background read does not clear a mutation error");
  assert.equal(input.value, "Retry this draft");
  assert.match(section.textContent, /External fact/);
});

for (const outcome of ["success", "failure"]) {
  test(`a refresh ${outcome} cannot replace a newer export status`, async t => {
    const refresh = deferred();
    let reads = 0;
    answering(t, {
      memory_list: () => ++reads === 1 ? state([]) : refresh.promise,
      memory_export: new Error("newer export failed"),
    });
    const section = memorySection();
    await settle();
    refreshMemorySection();
    button(section, "Export unencrypted JSON").fire("click");
    await settle();
    if (outcome === "success") refresh.resolve(state([item("chat", "External fact", "chat")]));
    else refresh.reject(new Error("older refresh failed"));
    await settle();
    assert.match(section.textContent, /newer export failed/);
    assert.doesNotMatch(section.textContent, /older refresh failed/);
  });
}

test("focus retries an initial read failure and clears only that read error after recovery", async t => {
  let reads = 0;
  answering(t, { memory_list: () => {
    if (++reads === 1) throw new Error("initial read failed");
    return state([item("chat", "Recovered fact", "chat")], false);
  } });
  const section = memorySection();
  await settle();
  const input = section.find("input")[0];
  input.value = "Preserve initial draft";
  assert.equal(button(section, "Add").disabled, true);
  assert.match(section.textContent, /initial read failed/);
  refreshMemorySection();
  await settle();
  assert.equal(button(section, "Add").disabled, false);
  assert.equal(input.value, "Preserve initial draft");
  assert.equal(section.find(".switch")[0].getAttribute("aria-pressed"), "false");
  assert.match(section.textContent, /Recovered fact/);
  assert.doesNotMatch(section.textContent, /initial read failed/);
});

for (const outcome of ["success", "failure"]) {
  test(`replacement retires the old section's late read ${outcome}, queued focus and confirmation timer`, async t => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const oldRead = deferred();
    let reads = 0;
    answering(t, { memory_list: () => [state([item("old", "Old section")]), oldRead.promise,
      state([item("new", "New section")]), state([item("chat", "Current chat fact", "chat")])][reads++] });
    const focus = settingsFocus();
    const oldSection = memorySection();
    await settle();
    const oldClear = button(oldSection, "Delete everything");
    oldClear.fire("click");
    focus();
    focus();
    const section = memorySection();
    await settle();
    section.find("input")[0].value = "Current draft";
    const oldContent = oldSection.textContent;
    if (outcome === "success") oldRead.resolve(state([item("late", "Retired response")]));
    else oldRead.reject(new Error("retired read failure"));
    await settle();
    t.mock.timers.tick(4000);
    assert.equal(oldSection.textContent, oldContent, "retired reads and timers do not redraw an old section");
    assert.equal(sent("memory_list").length, 3, "the retired section does not drain queued focus work");
    assert.match(section.textContent, /New section/);
    focus();
    await settle();
    assert.equal(sent("memory_list").length, 4, "the single listener targets only the newest section");
    assert.equal(section.find("input")[0].value, "Current draft");
    assert.match(section.textContent, /Current chat fact/);
  });
}

test("a replacement section waits for the retired section's submitted write before its initial read", async t => {
  const pending = deferred();
  let stored = state([item("one", "Original")]);
  answering(t, { memory_list: () => stored, memory_add: pending.promise });
  const oldSection = memorySection();
  await settle();
  oldSection.find("input")[0].value = "Submitted before redraw";
  button(oldSection, "Add").fire("click");
  const section = memorySection();
  await settle();
  assert.equal(sent("memory_list").length, 1, "a section redraw still shares the window's pending native writes");
  assert.equal(button(section, "Add").disabled, true);
  section.find("input")[0].value = "Replacement draft";
  stored = state([item("one", "Original"), item("added", "Submitted before redraw")]);
  pending.resolve(stored);
  await settle();
  assert.equal(sent("memory_list").length, 2);
  assert.match(section.textContent, /Submitted before redraw/);
  assert.equal(section.find("input")[0].value, "Replacement draft");
  assert.equal(button(section, "Add").disabled, false);
});

test("refresh preserves an unsaved editor even when its item was removed externally", async t => {
  let stored = state([item("one", "Original")]);
  answering(t, { memory_list: () => stored, memory_edit: new Error("This memory is gone.") });
  const section = memorySection();
  await settle();
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Do not discard my correction";
  stored = state([]);
  refreshMemorySection();
  await settle();
  assert.equal(editField(section), field);
  assert.equal(field.value, "Do not discard my correction");
  button(section, "Save").fire("click");
  await settle();
  assert.match(section.textContent, /This memory is gone/);
  assert.equal(editField(section), field);
  assert.equal(field.value, "Do not discard my correction");
  button(section, "Cancel").fire("click");
  assert.equal(editField(section), undefined);
  assert.match(section.textContent, /Nothing yet/);
});

for (const mutation of mutations.filter(({ name }) => ["Add", "toggle", "Delete"].includes(name))) {
  test(`an externally removed editor keeps its draft through a later unrelated ${mutation.name}`, async t => {
    const original = item("one", "Original");
    const other = item("other", "Other fact");
    let stored = state([original, other]);
    answering(t, { memory_list: () => stored, [mutation.command]: ({ text, enabled }) => {
      if (mutation.name === "Add") return state([other, item("added", text)]);
      if (mutation.name === "toggle") return state([other], enabled);
      return state([]);
    } });
    const section = memorySection();
    await settle();
    button(section, "Edit").fire("click");
    const field = editField(section);
    field.value = "Retained orphan draft";
    stored = state([other]);
    refreshMemorySection();
    await settle();
    assert.equal(editField(section), field);
    mutation.run(section);
    await settle();
    assert.equal(editField(section), field);
    assert.equal(field.value, "Retained orphan draft");
  });
}

for (const deletion of ["matching Delete", "Delete everything"]) {
  test(`an explicit ${deletion} still retires an externally removed editor`, async t => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const original = item("one", "Original");
    const other = item("other", "Other fact");
    let stored = state([original, other]);
    answering(t, { memory_list: () => stored, memory_delete: state([other]), memory_clear: state([]) });
    const section = memorySection();
    await settle();
    const oldDelete = button(section, "Delete");
    button(section, "Edit").fire("click");
    const field = editField(section);
    field.value = "Explicitly abandoned draft";
    stored = state([other]);
    refreshMemorySection();
    await settle();
    if (deletion === "matching Delete") oldDelete.fire("click");
    else {
      const clearBtn = button(section, "Delete everything");
      clearBtn.fire("click");
      clearBtn.fire("click");
    }
    await settle();
    assert.equal(editField(section), undefined);
  });
}

const eventOn = (field, type, details = {}) => {
  for (const listener of field.listeners.get(type) ?? []) listener({ type, ...details });
};
const scalarText = (pattern, length) => {
  const scalars = [...pattern];
  return Array.from({ length }, (_, index) => scalars[index % scalars.length]).join("");
};
const memoryDraft = (section, action) => {
  if (action === "Save") {
    button(section, "Edit").fire("click");
    return editField(section);
  }
  return section.find("input")[0];
};
// A fixture for the native clean/count replies, not execution of Rust. The
// actual DOM input and submit handlers below must preserve and send each draft.
function nativeMemoryReply({ text }) {
  const cleaned = text.replace(/[\u0000-\u001f\u007f-\u009f]/gu, " ").replace(/\s+/gu, " ").trim();
  if ([...cleaned].length > 500) throw new Error("A memory is at most 500 characters.");
  return state([item("one", cleaned)]);
}
function enterText(field, text, method) {
  if (method === "paste") {
    field.value = text;
    eventOn(field, "input", { inputType: "insertFromPaste", data: text, isComposing: false });
  } else {
    field.value = "";
    for (const scalar of text) {
      field.value += scalar;
      eventOn(field, "input", { inputType: "insertText", data: scalar, isComposing: false });
    }
  }
}

for (const action of ["Add", "Save"]) {
  test(`${action} has no UTF-16 maxlength that rejects a native-valid 500-emoji draft`, async t => {
    const text = "😀".repeat(500);
    answering(t, { memory_list: state([item("one", text)]) });
    const section = memorySection();
    await settle();
    const field = memoryDraft(section, action);
    assert.equal(field.getAttribute("maxlength"), null, "the native Unicode-scalar validator owns the length limit");
    if (action === "Save") assert.equal(field.getAttribute("value"), text, "an existing native-valid draft is not shortened on editing");
  });

  for (const [name, pattern] of [["ASCII", "x"], ["Japanese", "日"], ["emoji", "😀"], ["mixed combining text", "A日😀e\u0301"]]) {
    for (const method of ["typing", "paste"]) {
      test(`${action} after ${method} preserves 501 ${name} scalars on native rejection and accepts a corrected 500`, async t => {
        const command = action === "Add" ? "memory_add" : "memory_edit";
        answering(t, { memory_list: state([item("one", "Original")]), [command]: nativeMemoryReply });
        const section = memorySection();
        await settle();
        const field = memoryDraft(section, action);
        const long = scalarText(pattern, 501);
        enterText(field, long, method);
        assert.equal(field.value, long, "input handling never silently truncates a draft or surrogate pair");
        assert.equal(sent(command).length, 0, "typing and paste alone do not save");
        button(section, action).fire("click");
        await settle();
        assert.equal(sent(command)[0].text, long);
        assert.equal(field.value, long);
        if (action === "Save") assert.equal(editField(section), field);
        assert.equal(button(section, action).disabled, false, "native refusal releases submission for a corrected retry");
        assert.match(section.textContent, /A memory is at most 500 characters/);
        const valid = scalarText(pattern, 500);
        enterText(field, valid, method);
        key(field, "Enter");
        await settle();
        assert.equal(sent(command)[1].text, valid);
        assert.match(section.find(".memory-text")[0].textContent, new RegExp("^" + valid + "$", "u"));
        assert.doesNotMatch(section.textContent, /A memory is at most 500 characters/);
        if (action === "Add") assert.equal(field.value, "");
        else assert.equal(editField(section), undefined);
      });
    }
  }

  test(`${action} lets native whitespace normalization accept 500 scalars even when the raw draft is longer`, async t => {
    const command = action === "Add" ? "memory_add" : "memory_edit";
    answering(t, { memory_list: state([item("one", "Original")]), [command]: nativeMemoryReply });
    const section = memorySection();
    await settle();
    const field = memoryDraft(section, action);
    const raw = `  ${"😀".repeat(249)}   \u2003 ${"日".repeat(250)}  `;
    assert.ok([...raw].length > 500);
    enterText(field, raw, "paste");
    button(section, action).fire("click");
    await settle();
    const saved = section.find(".memory-text")[0].textContent;
    assert.equal(saved, `${"😀".repeat(249)} ${"日".repeat(250)}`);
    assert.equal([...saved].length, 500);
    assert.equal(sent(command)[0].text, action === "Add" ? raw.trim() : raw);
    assert.doesNotMatch(section.textContent, /A memory is at most 500 characters/);
  });

  test(`${action} leaves composition untouched and ignores Enter, Escape and synthetic submit until it ends`, async t => {
    const command = action === "Add" ? "memory_add" : "memory_edit";
    answering(t, { memory_list: state([item("one", "Original")]), [command]: nativeMemoryReply });
    const section = memorySection();
    await settle();
    const field = memoryDraft(section, action);
    const draft = "😀".repeat(501);
    eventOn(field, "compositionstart");
    field.value = draft;
    eventOn(field, "input", { inputType: "insertCompositionText", isComposing: true });
    key(field, "Enter");
    key(field, "Escape");
    button(section, action)?.fire("click");
    await settle();
    assert.equal(sent(command).length, 0, "composition confirmation is not a Memory submission");
    assert.equal(field.value, draft);
    if (action === "Save") assert.equal(editField(section), field, "Escape belongs to the IME while composing");
    field.value = "日本語の確定した文";
    eventOn(field, "compositionend");
    eventOn(field, "input", { inputType: "insertText", isComposing: false });
    assert.equal(sent(command).length, 0, "compositionend never automatically saves");
    key(field, "Enter");
    await settle();
    assert.equal(sent(command).length, 1);
    assert.equal(sent(command)[0].text, "日本語の確定した文");
  });

  for (const keyDetails of [{ isComposing: true }, { keyCode: 229 }]) {
    test(`${action} ignores an IME Enter flagged ${Object.keys(keyDetails)[0]} even without compositionstart`, async t => {
      const command = action === "Add" ? "memory_add" : "memory_edit";
      answering(t, { memory_list: state([item("one", "Original")]), [command]: nativeMemoryReply });
      const section = memorySection();
      await settle();
      const field = memoryDraft(section, action);
      field.value = "Uncommitted IME draft";
      eventOn(field, "keydown", { key: "Enter", ...keyDetails });
      await settle();
      assert.equal(sent(command).length, 0);
      assert.equal(field.value, "Uncommitted IME draft");
      key(field, "Enter");
      await settle();
      assert.equal(sent(command).length, 1);
    });
  }

  test(`${action} completion does not clear or close a field with a newly started composition`, async t => {
    const command = action === "Add" ? "memory_add" : "memory_edit";
    const pending = deferred();
    answering(t, { memory_list: state([item("one", "Original")]), [command]: pending.promise });
    const section = memorySection();
    await settle();
    const field = memoryDraft(section, action);
    field.value = "Submitted text";
    button(section, action).fire("click");
    eventOn(field, "compositionstart");
    // Some IMEs have not changed the DOM value yet, but still own the field.
    pending.resolve(state([item("one", "Submitted text")]));
    await settle();
    assert.equal(field.value, "Submitted text", "do not replace a composing field's value");
    if (action === "Save") assert.equal(editField(section), field, "do not retire the composing editor");
    field.value = "次の入力";
    eventOn(field, "compositionend");
    assert.equal(field.value, "次の入力");
    assert.equal(sent(command).length, 1);
  });
}

for (const update of ["refresh", "save", "toggle"]) {
  test(`a ${update} redraw waits for compositionend before detaching the active editor`, async t => {
    const pending = deferred();
    let stored = state([item("one", "Original")]);
    answering(t, {
      memory_list: () => stored,
      memory_edit: pending.promise,
      memory_set_enabled: ({ enabled }) => state(stored.items, enabled),
    });
    const section = memorySection();
    await settle();
    button(section, "Edit").fire("click");
    const field = editField(section);
    field.value = "Submitted text";
    if (update === "save") button(section, "Save").fire("click");
    const row = section.find(".memory-item")[0];
    const list = section.find(".memory-list")[0];
    const remove = list.removeChild.bind(list);
    let detached = 0;
    t.mock.method(list, "removeChild", node => {
      if (node === row) detached++;
      return remove(node);
    });
    eventOn(field, "compositionstart");
    field.value = "Composing current draft";
    stored = state([item("one", "Submitted text"), item("chat", "New chat fact", "chat")]);
    if (update === "refresh") refreshMemorySection();
    else if (update === "save") pending.resolve(stored);
    else section.find(".switch")[0].fire("click");
    await settle();
    assert.equal(detached, 0, "detaching a focused input can cancel the browser's IME session");
    assert.equal(editField(section), field);
    assert.equal(field.value, "Composing current draft");
    eventOn(field, "compositionend");
    assert.equal(editField(section), field);
    assert.equal(field.value, "Composing current draft");
    assert.match(section.textContent, /New chat fact/, "the retained authoritative state is drawn after composition ends");
  });
}

// Execute the page's actual registration through the real event bridge. This
// avoids booting unrelated Settings sections while still testing its promise
// ordering, rejection handling and the event delivered by Tauri's test fixture.
function settingsMemoryEvents() {
  const source = readFileSync(new URL("../src/settings/main.ts", import.meta.url), "utf8");
  const registrations = source.match(/void onEvent\("memory-changed", refreshMemorySection\)[\s\S]*?;\n/g) ?? [];
  assert.equal(registrations.length, 1, "one page-level subscription, not one per section/render");
  assert.ok(source.indexOf(registrations[0]) < source.indexOf("async function main()"));
  return new Function("onEvent", "refreshMemorySection", registrations[0].replace(/^void /, "return "))(onEvent, refreshMemorySection);
}

test("post-save memory-changed reveals a chat fact when focus read before remember persistence", async t => {
  let stored = state([]);
  answering(t, { memory_list: () => stored });
  const section = memorySection();
  await settle();
  await settingsMemoryEvents();
  await settle();
  const focus = settingsFocus();
  focus();
  await settle();
  assert.equal(section.find(".memory-item").length, 0, "focus happened while the native write was still pending");
  stored = state([item("chat", "Confirmed after focus", "chat")]);
  emit("memory-changed", null);
  await settle();
  assert.match(section.textContent, /Confirmed after focus/);
  assert.equal(section.find(".memory-badge")[0].textContent, "From the chat");
});

test("listener installation refreshes an initial snapshot taken before a missed save notification", async t => {
  const installed = deferred();
  let stored = state([]);
  answering(t, { memory_list: () => stored });
  const invoke = internals.invoke;
  let registrations = 0;
  t.mock.method(internals, "invoke", async (command, args) => {
    if (command === "plugin:event|listen" && args.event === "memory-changed") {
      registrations++;
      await installed.promise;
    }
    return invoke(command, args);
  });
  const section = memorySection();
  const registering = settingsMemoryEvents();
  await settle();
  assert.equal(sent("memory_list").length, 1);
  stored = state([item("chat", "Saved before listener installation", "chat")]);
  // This native notification is intentionally absent: no listener was ready.
  installed.resolve();
  await registering;
  await settle();
  assert.equal(registrations, 1);
  assert.equal(sent("memory_list").length, 2);
  assert.match(section.textContent, /Saved before listener installation/);
});

test("memory event registration rejection is handled and focus remains a working fallback", async t => {
  let stored = state([]);
  answering(t, { memory_list: () => stored });
  const invoke = internals.invoke;
  t.mock.method(internals, "invoke", async (command, args) => {
    if (command === "plugin:event|listen" && args.event === "memory-changed") throw new Error("listener unavailable");
    return invoke(command, args);
  });
  const warnings = [];
  t.mock.method(console, "warn", (...args) => warnings.push(args));
  const section = memorySection();
  await settle();
  await assert.doesNotReject(settingsMemoryEvents());
  assert.equal(warnings.length, 1);
  assert.match(warnings[0].join(" "), /listener unavailable/);
  stored = state([item("chat", "Visible through focus fallback", "chat")]);
  settingsFocus()();
  await settle();
  assert.match(section.textContent, /Visible through focus fallback/);
});

test("duplicate memory events during an older read coalesce and discard its stale snapshot", async t => {
  const pending = deferred();
  let nextRead = null;
  let stored = state([]);
  answering(t, { memory_list: () => {
    const read = nextRead;
    nextRead = null;
    return read ?? stored;
  } });
  const section = memorySection();
  await settle();
  await settingsMemoryEvents();
  await settle();
  const baseline = sent("memory_list").length;
  nextRead = pending.promise;
  settingsFocus()();
  stored = state([item("chat", "Persisted after the old read", "chat")]);
  emit("memory-changed", null);
  emit("memory-changed", null);
  emit("memory-changed", null);
  assert.equal(sent("memory_list").length, baseline + 1, "the active read is not overlapped");
  pending.resolve(state([item("stale", "Stale pre-save snapshot")]));
  await settle();
  assert.equal(sent("memory_list").length, baseline + 2, "duplicate events request only one later snapshot");
  assert.doesNotMatch(section.textContent, /Stale pre-save snapshot/);
  assert.match(section.textContent, /Persisted after the old read/);
});

test("memory-changed during a local write waits for it and preserves the next Add draft", async t => {
  const pending = deferred();
  let stored = state([]);
  answering(t, { memory_list: () => stored, memory_add: pending.promise });
  const section = memorySection();
  await settle();
  await settingsMemoryEvents();
  await settle();
  const baseline = sent("memory_list").length;
  const input = section.find("input")[0];
  input.value = "Local submitted fact";
  button(section, "Add").fire("click");
  input.value = "Next Add draft";
  stored = state([item("local", "Local submitted fact"), item("chat", "Confirmed in chat", "chat")]);
  emit("memory-changed", null);
  emit("memory-changed", null);
  assert.equal(sent("memory_list").length, baseline);
  pending.resolve(state([item("local", "Local submitted fact")]));
  await settle();
  assert.equal(sent("memory_list").length, baseline + 1);
  assert.equal(input.value, "Next Add draft");
  assert.match(section.textContent, /Local submitted fact/);
  assert.match(section.textContent, /Confirmed in chat/);
});

test("memory-changed retains both drafts and never detaches a composing editor", async t => {
  let stored = state([item("one", "Original")]);
  answering(t, { memory_list: () => stored });
  const section = memorySection();
  await settle();
  await settingsMemoryEvents();
  await settle();
  const input = section.find("input")[0];
  input.value = "Next Add draft";
  button(section, "Edit").fire("click");
  const field = editField(section);
  field.value = "Composing edit draft";
  eventOn(field, "compositionstart");
  const list = section.find(".memory-list")[0];
  const remove = list.removeChild.bind(list);
  let detached = 0;
  t.mock.method(list, "removeChild", node => { detached++; return remove(node); });
  stored = state([item("one", "Original"), item("chat", "Chat fact during composition", "chat")]);
  emit("memory-changed", null);
  await settle();
  assert.equal(detached, 0);
  assert.equal(input.value, "Next Add draft");
  assert.equal(editField(section), field);
  assert.equal(field.value, "Composing edit draft");
  eventOn(field, "compositionend");
  assert.match(section.textContent, /Chat fact during composition/);
  assert.equal(input.value, "Next Add draft");
  assert.equal(field.value, "Composing edit draft");
});

for (const outcome of ["success", "failure"]) {
  test(`section replacement retains one memory listener and ignores its old read ${outcome}`, async t => {
    const pending = deferred();
    let nextRead = null;
    let stored = state([item("old", "Old section")]);
    answering(t, { memory_list: () => {
      const read = nextRead;
      nextRead = null;
      return read ?? stored;
    } });
    const invoke = internals.invoke;
    let registrations = 0;
    t.mock.method(internals, "invoke", (command, args) => {
      if (command === "plugin:event|listen" && args.event === "memory-changed") registrations++;
      return invoke(command, args);
    });
    memorySection();
    await settle();
    await settingsMemoryEvents();
    await settle();
    nextRead = pending.promise;
    emit("memory-changed", null);
    emit("memory-changed", null);
    stored = state([item("new", "Replacement section")]);
    const section = memorySection();
    await settle();
    const baseline = sent("memory_list").length;
    section.find("input")[0].value = "Replacement draft";
    if (outcome === "success") pending.resolve(state([item("old", "Late old read")]));
    else pending.reject(new Error("late old read error"));
    await settle();
    assert.equal(sent("memory_list").length, baseline, "retired queued reads do not run");
    assert.match(section.textContent, /Replacement section/);
    assert.doesNotMatch(section.textContent, /Late old read|late old read error/);
    stored = state([item("chat", "Newest chat fact", "chat")]);
    emit("memory-changed", null);
    await settle();
    assert.equal(registrations, 1);
    assert.equal(sent("memory_list").length, baseline + 1);
    assert.equal(section.find("input")[0].value, "Replacement draft");
    assert.match(section.textContent, /Newest chat fact/);
  });
}
