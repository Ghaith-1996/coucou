import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, sent, internals } from "./tauri.mjs";
import { memorySection } from "../src/settings/memory.ts";
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
