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
  let adding = false;
  type Editor = { id: string; field: HTMLInputElement; save: HTMLButtonElement; el: HTMLElement };
  let editing: Editor | null = null;
  // A canceled editor can still own a native write. Its replacement may be
  // typed into, but must wait before submitting another edit.
  let saving: Editor | null = null;
  let confirmingClear = false;
  let clearing = false;
  let clearTimer: ReturnType<typeof setTimeout> | null = null;

  function resetClear() {
    confirmingClear = false;
    if (clearTimer !== null) clearTimeout(clearTimer);
    clearTimer = null;
  }

  const fail = (err: unknown) => {
    note.textContent = String(err).replace(/^Error:\s*/, "");
  };
  const apply = (next: MemoryState | null | undefined) => {
    if (next) {
      state = next;
      loaded = true;
      resetClear();
    }
    draw();
  };

  function beginEdit(item: MemoryItem) {
    const field = h("input", { type: "text", value: item.text, maxlength: "500", style: "flex:1" });
    const save = h("button", { text: STRINGS.save });
    const cancelBtn = h("button", { text: STRINGS.cancel });
    const editor: Editor = { id: item.id, field, save, el: h("div", { class: "row memory-item" }, field, save, cancelBtn) };
    const cancel = () => {
      if (editing !== editor) return;
      editing = null;
      draw();
    };
    const commit = async () => {
      if (editing !== editor || saving) return;
      const draft = field.value;
      saving = editor;
      save.disabled = true;
      try {
        const next = await Bridge.memoryEdit(editor.id, draft);
        if (editing === editor) {
          if (field.value === draft) editing = null;
          note.textContent = "";
        }
        // The saved result still updates the list after Cancel, but never
        // replaces a newer editor's field or clears its draft/status.
        apply(next);
      } catch (err) {
        if (editing === editor) fail(err);
      } finally {
        saving = null;
        if (editing) editing.save.disabled = false;
      }
    };
    save.addEventListener("click", commit);
    cancelBtn.addEventListener("click", cancel);
    field.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void commit();
      if (e.key === "Escape") cancel();
    });
    editing = editor;
    draw();
    requestAnimationFrame(() => { if (editing === editor) field.focus(); });
  }

  function row(item: MemoryItem): HTMLElement {
    if (editing?.id === item.id) return editing.el;
    return h("div", { class: "row memory-item" },
      h("span", { class: "memory-text", text: item.text }),
      item.source === "chat" ? h("span", { class: "memory-badge", text: STRINGS.fromChat }) : null,
      h("button", { text: STRINGS.edit, onclick: () => beginEdit(item) }),
      h("button", { class: "danger", text: STRINGS.remove, onclick: () => Bridge.memoryDelete(item.id).then(apply, fail) }),
    );
  }

  function draw() {
    input.disabled = !loaded;
    addBtn.disabled = !loaded || adding;
    useSwitch.disabled = !loaded || toggling;
    useSwitch.classList.toggle("on", state.enabled);
    useSwitch.setAttribute("aria-pressed", String(state.enabled));
    if (editing && !state.items.some(item => item.id === editing?.id)) editing = null;
    if (editing) editing.save.disabled = saving !== null;
    // Reuse the draft field through unrelated redraws, including its focus
    // and selection if rebuilding the surrounding list detaches it.
    const focused = editing && editing.field === document.activeElement ? editing.field : null;
    const selection = focused ? [focused.selectionStart, focused.selectionEnd] as const : null;
    clear(list);
    if (!state.items.length) list.append(h("div", { class: "hint", text: STRINGS.empty }));
    for (const item of state.items) list.append(row(item));
    if (focused && selection) {
      focused.focus();
      focused.setSelectionRange(selection[0], selection[1]);
    }
    clearBtn.disabled = !loaded || clearing;
    clearBtn.textContent = confirmingClear ? STRINGS.confirmClear : STRINGS.clearAll;
    clearBtn.hidden = !state.items.length;
  }

  const add = async () => {
    // An older initial list must not arrive after and overwrite an add result.
    // Reserve submission, but leave the input available for the next draft.
    if (!loaded || adding) return;
    const draft = input.value;
    const text = draft.trim();
    if (!text) return;
    adding = true;
    addBtn.disabled = true;
    try {
      const next = await Bridge.memoryAdd(text);
      if (input.value === draft) input.value = "";
      note.textContent = "";
      apply(next);
    } catch (err) {
      fail(err);
    } finally {
      adding = false;
      addBtn.disabled = !loaded;
    }
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
  // Two clicks for the current state. An in-flight deletion cannot re-arm
  // confirmation for items added later, and a changed state requires asking again.
  clearBtn.addEventListener("click", async () => {
    if (!loaded || clearing || !state.items.length) return;
    if (!confirmingClear) {
      confirmingClear = true;
      draw();
      clearTimer = setTimeout(() => {
        clearTimer = null;
        confirmingClear = false;
        draw();
      }, 4000);
      return;
    }
    resetClear();
    clearing = true;
    draw();
    try {
      apply(await Bridge.memoryClear());
      note.textContent = "";
    } catch (err) {
      fail(err);
    } finally {
      clearing = false;
      resetClear();
      draw();
    }
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
