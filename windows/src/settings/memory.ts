// Settings → Memory: what the chat knows about the user (memory.rs). Every
// item can be read, edited and deleted here, all of them at once too, and
// exported as a plain JSON file. A switch stops the chat from using memory
// without deleting it. Items the chat proposed are marked.

import { Bridge, type MemoryItem, type MemoryState } from "../core/bridge";
import { h, clear } from "../views/dom";
import { t } from "../i18n/i18n";

const STRINGS = {
  get title() { return t("Memory"); },
  get hint() { return t("What the chat knows about you, in every conversation. Kept on this PC, encrypted for your Windows account. The chat can propose an item; nothing is saved until you confirm it."); },
  get sent() { return t("When it is on, it goes with every question to the model you chat with, local or online."); },
  get use() { return t("Use memory in the chat"); },
  get placeholder() { return t("Something the chat should remember…"); },
  get add() { return t("Add"); },
  get edit() { return t("Edit"); },
  get save() { return t("Save"); },
  get cancel() { return t("Cancel"); },
  get remove() { return t("Delete"); },
  get fromChat() { return t("From the chat"); },
  get empty() { return t("Nothing yet."); },
  get exportFile() { return t("Export unencrypted JSON"); },
  get exportHint() { return t("Export saves an unencrypted JSON file in Downloads. Anyone with access to that file can read your memories."); },
  get clearAll() { return t("Delete everything"); },
  get confirmClear() { return t("Really delete everything?"); },
  exported: (path: string) => t("Saved to {path}", { path }),
};

export function memorySection(): HTMLElement {
  const list = h("div", { class: "memory-list" });
  const note = h("div", { class: "hint" });
  const input = h("input", { type: "text", placeholder: STRINGS.placeholder, maxlength: "500", style: "flex:1" }) as HTMLInputElement;
  const useSwitch = h("button", { class: "switch", "aria-label": STRINGS.use }) as HTMLButtonElement;
  const clearBtn = h("button", { class: "danger", text: STRINGS.clearAll });
  const addBtn = h("button", { text: STRINGS.add });
  let state: MemoryState = { version: 1, enabled: true, items: [] };
  let loaded = false;
  let toggling = false;
  let editing: string | null = null;
  let confirmingClear = false;

  const fail = (err: unknown) => {
    note.textContent = String(err).replace(/^Error:\s*/, "");
  };
  const apply = (next: MemoryState | null | undefined) => {
    if (next) {
      state = next;
      loaded = true;
    }
    draw();
  };

  function row(item: MemoryItem): HTMLElement {
    if (editing === item.id) {
      const field = h("input", { type: "text", value: item.text, maxlength: "500", style: "flex:1" }) as HTMLInputElement;
      const commit = () => {
        Bridge.memoryEdit(item.id, field.value).then((next) => {
          editing = null;
          note.textContent = "";
          apply(next);
        }, fail);
      };
      field.addEventListener("keydown", (e) => {
        if (e.key === "Enter") commit();
        if (e.key === "Escape") {
          editing = null;
          draw();
        }
      });
      requestAnimationFrame(() => field.focus());
      return h("div", { class: "row memory-item" },
        field,
        h("button", { text: STRINGS.save, onclick: commit }),
        h("button", { text: STRINGS.cancel, onclick: () => { editing = null; draw(); } }),
      );
    }
    return h("div", { class: "row memory-item" },
      h("span", { class: "memory-text", text: item.text }),
      item.source === "chat" ? h("span", { class: "memory-badge", text: STRINGS.fromChat }) : null,
      h("button", { text: STRINGS.edit, onclick: () => { editing = item.id; draw(); } }),
      h("button", { class: "danger", text: STRINGS.remove, onclick: () => Bridge.memoryDelete(item.id).then(apply, fail) }),
    );
  }

  function draw() {
    input.disabled = !loaded;
    addBtn.disabled = !loaded;
    useSwitch.disabled = !loaded || toggling;
    useSwitch.classList.toggle("on", state.enabled);
    useSwitch.setAttribute("aria-pressed", String(state.enabled));
    clear(list);
    if (!state.items.length) list.append(h("div", { class: "hint", text: STRINGS.empty }));
    for (const item of state.items) list.append(row(item));
    clearBtn.textContent = confirmingClear ? STRINGS.confirmClear : STRINGS.clearAll;
    clearBtn.hidden = !state.items.length;
  }

  const add = () => {
    // An older initial list must not arrive after and overwrite an add result.
    // Guard both keyboard and synthetic clicks, not only disabled controls.
    if (!loaded) return;
    const text = input.value.trim();
    if (!text) return;
    Bridge.memoryAdd(text).then((next) => {
      input.value = "";
      note.textContent = "";
      apply(next);
    }, fail);
  };
  addBtn.addEventListener("click", add);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") add();
  });
  useSwitch.addEventListener("click", async () => {
    // Neither the initial default nor an in-flight request is authoritative.
    // Guard the handler too, so synthetic/repeated clicks cannot enqueue writes.
    if (!loaded || toggling) return;
    toggling = true;
    draw();
    try {
      apply(await Bridge.memorySetEnabled(!state.enabled));
      note.textContent = "";
    } catch (err) {
      fail(err);
    } finally {
      toggling = false;
      draw();
    }
  });
  // Two clicks: the first asks, the second deletes (the question goes after 4 s).
  clearBtn.addEventListener("click", () => {
    if (!confirmingClear) {
      confirmingClear = true;
      draw();
      setTimeout(() => {
        confirmingClear = false;
        draw();
      }, 4000);
      return;
    }
    confirmingClear = false;
    Bridge.memoryClear().then(apply, fail);
  });
  const exportBtn = h("button", {
    text: STRINGS.exportFile,
    onclick: () => Bridge.memoryExport().then((path) => { note.textContent = STRINGS.exported(path); }, fail),
  });

  Bridge.memoryList().then(apply, fail);
  draw();

  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: STRINGS.title })),
    h("div", { class: "hint", text: STRINGS.hint }),
    h("div", { class: "hint", text: STRINGS.sent }),
    h("div", { class: "row" }, h("label", { text: STRINGS.use }), useSwitch),
    h("div", { class: "row" }, input, addBtn),
    list,
    h("div", { class: "hint", text: STRINGS.exportHint }),
    h("div", { class: "row", style: "gap:8px" }, exportBtn, clearBtn),
    note,
  );
}
