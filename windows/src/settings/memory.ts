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

let activeSection: { refresh: () => void; resume: () => void; dispose: () => void } | null = null;
// Native writes outlive a language-driven section replacement. Its new
// initial read must wait for those writes too.
let mutationRevision = 0;
let pendingMutations = 0;

/** The Settings window calls this on focus, including after a chat addition. */
export function refreshMemorySection() {
  activeSection?.refresh();
}

function memoryInput(attrs: { value?: string; placeholder?: string }) {
  // HTML maxlength counts UTF-16 units, while memory.rs cleans the text and
  // permits 500 Unicode scalars. Let native validation reject an overlong
  // draft without truncating it or interfering with an IME's composition.
  const field = h("input", { type: "text", style: "flex:1", ...attrs });
  let composing = false;
  field.addEventListener("compositionstart", () => { composing = true; });
  field.addEventListener("compositionend", () => { composing = false; });
  return {
    field,
    isComposing: (event?: KeyboardEvent) => composing || event?.isComposing === true || event?.keyCode === 229,
  };
}

export function memorySection(): HTMLElement {
  activeSection?.dispose();
  const list = h("div", { class: "memory-list" });
  const note = h("div", { class: "hint" });
  const entry = memoryInput({ placeholder: STRINGS.placeholder });
  const input = entry.field;
  const useSwitch = h("button", { class: "switch", "aria-label": STRINGS.use }) as HTMLButtonElement;
  const clearBtn = h("button", { class: "danger", text: STRINGS.clearAll });
  const addBtn = h("button", { text: STRINGS.add });
  let state: MemoryState = { version: 1, enabled: true, items: [] };
  let loaded = false;
  let toggling = false;
  let adding = false;
  type Editor = { id: string; field: HTMLInputElement; save: HTMLButtonElement; el: HTMLElement; isComposing: () => boolean };
  let editing: Editor | null = null;
  // A canceled editor can still own a native write. Its replacement may be
  // typed into, but must wait before submitting another edit.
  let saving: Editor | null = null;
  let confirmingClear = false;
  let clearing = false;
  let clearTimer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;
  let refreshing = false;
  let refreshQueued = false;
  let noteRevision = 0;
  let readErrorRevision: number | null = null;

  function resetClear() {
    confirmingClear = false;
    if (clearTimer !== null) clearTimeout(clearTimer);
    clearTimer = null;
  }

  const showNote = (text: string) => {
    note.textContent = text;
    noteRevision++;
    readErrorRevision = null;
  };
  const fail = (err: unknown) => showNote(String(err).replace(/^Error:\s*/, ""));
  const apply = (next: MemoryState | null | undefined, retireEditor = false) => {
    if (next) {
      state = next;
      loaded = true;
      resetClear();
      if (retireEditor) editing = null;
    }
    draw();
  };

  // Serialize reads, not writes. A focus during a read/write queues one fresh
  // snapshot; reads that overlap a mutation can never roll back its result.
  async function refresh() {
    if (disposed) return;
    if (refreshing || pendingMutations) {
      refreshQueued = true;
      return;
    }
    refreshing = true;
    refreshQueued = false;
    const revision = mutationRevision;
    const message = noteRevision;
    try {
      const next = await Bridge.memoryList();
      if (disposed || revision !== mutationRevision || refreshQueued) return;
      apply(next);
      // A background read owns only its own prior failure message, never an
      // Add/Edit/Delete/Export status shown while the user keeps working.
      if (next && readErrorRevision === noteRevision) showNote("");
    } catch (err) {
      if (!disposed && revision === mutationRevision && !refreshQueued && message === noteRevision) {
        fail(err);
        readErrorRevision = noteRevision;
      }
    } finally {
      refreshing = false;
      if (!disposed && refreshQueued && !pendingMutations) void refresh();
    }
  }

  function beginMutation() {
    mutationRevision++;
    pendingMutations++;
    if (refreshing) refreshQueued = true;
  }

  function endMutation() {
    pendingMutations--;
    activeSection?.resume();
  }

  function beginEdit(item: MemoryItem) {
    if (disposed) return;
    const editorInput = memoryInput({ value: item.text });
    const field = editorInput.field;
    const save = h("button", { text: STRINGS.save });
    const cancelBtn = h("button", { text: STRINGS.cancel });
    const editor: Editor = { id: item.id, field, save, el: h("div", { class: "row memory-item" }, field, save, cancelBtn), isComposing: editorInput.isComposing };
    const cancel = () => {
      if (editing !== editor) return;
      editing = null;
      draw();
    };
    const commit = async () => {
      if (disposed || editing !== editor || saving || editorInput.isComposing()) return;
      const draft = field.value;
      saving = editor;
      save.disabled = true;
      beginMutation();
      try {
        const next = await Bridge.memoryEdit(editor.id, draft);
        if (editing === editor) {
          if (field.value === draft && !editorInput.isComposing()) editing = null;
          showNote("");
        }
        // The saved result still updates the list after Cancel, but never
        // replaces a newer editor's field or clears its draft/status.
        apply(next);
      } catch (err) {
        if (editing === editor) fail(err);
      } finally {
        saving = null;
        if (editing) editing.save.disabled = false;
        endMutation();
      }
    };
    save.addEventListener("click", commit);
    cancelBtn.addEventListener("click", cancel);
    field.addEventListener("keydown", (e) => {
      if (editorInput.isComposing(e)) return;
      if (e.key === "Enter") void commit();
      if (e.key === "Escape") cancel();
    });
    field.addEventListener("compositionend", () => { if (editing === editor && !disposed) draw(); });
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
      h("button", { class: "danger", text: STRINGS.remove, onclick: async () => {
        if (disposed) return;
        beginMutation();
        try { apply(await Bridge.memoryDelete(item.id), editing?.id === item.id); }
        catch (err) { fail(err); }
        finally { endMutation(); }
      } }),
    );
  }

  function draw() {
    input.disabled = !loaded;
    addBtn.disabled = !loaded || adding;
    useSwitch.disabled = !loaded || toggling;
    useSwitch.classList.toggle("on", state.enabled);
    useSwitch.setAttribute("aria-pressed", String(state.enabled));
    if (editing) editing.save.disabled = saving !== null;
    // Even detaching/reinserting the same field can end an IME session. Keep
    // the new state, but defer rebuilding its list until compositionend.
    if (!editing?.isComposing()) {
      const focused = editing && editing.field === document.activeElement ? editing.field : null;
      const selection = focused ? [focused.selectionStart, focused.selectionEnd] as const : null;
      clear(list);
      if (!state.items.length && !editing) list.append(h("div", { class: "hint", text: STRINGS.empty }));
      for (const item of state.items) list.append(row(item));
      // An externally deleted item's unsaved field remains available to copy.
      if (editing && !state.items.some(item => item.id === editing?.id)) list.append(editing.el);
      if (focused && selection) {
        focused.focus();
        focused.setSelectionRange(selection[0], selection[1]);
      }
    }
    clearBtn.disabled = !loaded || clearing;
    clearBtn.textContent = confirmingClear ? STRINGS.confirmClear : STRINGS.clearAll;
    clearBtn.hidden = !state.items.length;
  }

  const add = async () => {
    // An older initial list must not arrive after and overwrite an add result.
    // Reserve submission, but leave the input available for the next draft.
    if (disposed || !loaded || adding || entry.isComposing()) return;
    const draft = input.value;
    const text = draft.trim();
    if (!text) return;
    adding = true;
    addBtn.disabled = true;
    beginMutation();
    try {
      const next = await Bridge.memoryAdd(text);
      if (input.value === draft && !entry.isComposing()) input.value = "";
      showNote("");
      apply(next);
    } catch (err) {
      fail(err);
    } finally {
      adding = false;
      addBtn.disabled = !loaded;
      endMutation();
    }
  };
  addBtn.addEventListener("click", add);
  input.addEventListener("keydown", (e) => {
    if (entry.isComposing(e)) return;
    if (e.key === "Enter") add();
  });
  useSwitch.addEventListener("click", async () => {
    // Neither the initial default nor an in-flight request is authoritative.
    // Guard the handler too, so synthetic/repeated clicks cannot enqueue writes.
    if (disposed || !loaded || toggling) return;
    toggling = true;
    draw();
    beginMutation();
    try {
      apply(await Bridge.memorySetEnabled(!state.enabled));
      showNote("");
    } catch (err) {
      fail(err);
    } finally {
      toggling = false;
      draw();
      endMutation();
    }
  });
  // Two clicks for the current state. An in-flight deletion cannot re-arm
  // confirmation for items added later, and a changed state requires asking again.
  clearBtn.addEventListener("click", async () => {
    if (disposed || !loaded || clearing || !state.items.length) return;
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
    beginMutation();
    try {
      apply(await Bridge.memoryClear(), true);
      showNote("");
    } catch (err) {
      fail(err);
    } finally {
      clearing = false;
      resetClear();
      draw();
      endMutation();
    }
  });
  const exportBtn = h("button", {
    text: STRINGS.exportFile,
    onclick: () => {
      if (disposed) return;
      noteRevision++;
      void Bridge.memoryExport().then((path) => showNote(STRINGS.exported(path)), fail);
    },
  });

  activeSection = {
    refresh: () => void refresh(),
    resume: () => { if (refreshQueued) void refresh(); },
    dispose: () => { disposed = true; resetClear(); },
  };
  void refresh();
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
