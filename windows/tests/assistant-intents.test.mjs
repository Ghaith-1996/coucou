// The intent parser (src/assistant/intents.ts) against the Mac's own table,
// tests/IntentParserTests.swift, converted case for case into
// fixtures/mac-intent-cases.json. Like the Mac's PillFixture, the pills are
// the whole catalog.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { normalise, parseIntent, parseMultiAction, parseTranscript, resolvePill } from "../src/assistant/intents.ts";
import { PILL_CATALOG, availablePills } from "../src/core/pills.ts";

const cases = JSON.parse(readFileSync(new URL("./fixtures/mac-intent-cases.json", import.meta.url), "utf8"));

test("the Mac's table was converted whole", () => {
  assert.equal(cases.length, 165);
});

for (const c of cases) {
  test(`Mac: ${c.label}`, () => {
    const pills = c.pills ? PILL_CATALOG : [];
    switch (c.type) {
      case "one":
        assert.deepEqual(parseIntent(c.text, pills), c.want);
        break;
      case "search": {
        const got = parseIntent(c.text, pills);
        assert.equal(got.kind, "musicPlaySearch");
        assert.equal(normalise(got.name), c.want);
        break;
      }
      case "playlist": {
        const got = parseIntent(c.text, pills);
        assert.equal(got.kind, "musicPlayPlaylist");
        assert.equal(normalise(got.name), c.want);
        break;
      }
      case "multi":
        assert.deepEqual(parseMultiAction(c.text, PILL_CATALOG), c.want);
        break;
      case "multiNil":
        assert.equal(parseMultiAction(c.text, PILL_CATALOG), null);
        break;
    }
  });
}

// ── Windows ───────────────────────────────────────────────────────────────────

test("Whisper's punctuation doesn't get in the way", () => {
  assert.deepEqual(parseTranscript("Ajoute GitHub.", PILL_CATALOG), [{ kind: "pillAdd", id: "integration_github" }]);
  assert.deepEqual(parseTranscript("Ajoute GitHub, s'il te plaît.", PILL_CATALOG), [{ kind: "pillAdd", id: "integration_github" }]);
  assert.deepEqual(parseTranscript("Est-ce que tu peux enlever Vercel ?", PILL_CATALOG), [{ kind: "pillRemove", id: "integration_vercel" }]);
  assert.deepEqual(parseTranscript("Add GitHub, then remove Vercel.", PILL_CATALOG), [
    { kind: "pillAdd", id: "integration_github" },
    { kind: "pillRemove", id: "integration_vercel" },
  ]);
});

test("ordinary dictation is not a command", () => {
  for (const text of [
    "Hello Mochi, please open the chat and show me the latest pull request.",
    "Remind me to review the voice capture branch tomorrow morning.",
    "Write a short summary of the failing tests for the team.",
    "",
  ]) {
    assert.deepEqual(parseTranscript(text, PILL_CATALOG), [], text);
  }
});

test("only the pills this build offers resolve", () => {
  const windows = availablePills("windows");
  const spotify = PILL_CATALOG.find((p) => p.id === "integration_spotify");
  assert.ok(spotify, "the catalog has Spotify");
  // Spotify is a Linux pill: on Windows "ajoute Spotify" names nothing.
  assert.equal(resolvePill("Spotify", windows), windows.some((p) => p.id === "integration_spotify") ? "integration_spotify" : null);
  assert.equal(resolvePill("GitHub", windows), "integration_github");
});

test("fuzzy matching, and refusing to guess between two", () => {
  assert.equal(resolvePill("vercell", PILL_CATALOG), "integration_vercel");
  assert.equal(resolvePill("guitar hub", PILL_CATALOG), "integration_github");
  assert.equal(resolvePill("zzzzzz", PILL_CATALOG), null);
});

// ── From intents to a pill plan (pill-command.ts) ─────────────────────────────

import { planPills } from "../src/assistant/pill-command.ts";

const declared = (main, ...active) => ({ mainPill: main, activeIntegrations: active });
const plan = (text, current) => planPills(parseTranscript(text, availablePills("windows")), current, "windows");

test("a pill command becomes the whole result, labelled", () => {
  const got = plan("Ajoute GitHub.", declared("integration_claude", "integration_vercel"));
  assert.equal(got.ok, true);
  assert.equal(got.label, "Add GitHub");
  assert.deepEqual(got.next, declared("integration_claude", "integration_vercel", "integration_github"));
});

test("several commands in one sentence make one plan", () => {
  const got = plan("Add GitHub, then remove Vercel.", declared("integration_claude", "integration_vercel"));
  assert.equal(got.ok, true);
  assert.equal(got.label, "Remove Vercel · Add GitHub");
  assert.deepEqual(got.next, declared("integration_claude", "integration_github"));
});

test("what can't be done says why, and changes nothing", () => {
  const full = declared("integration_claude", "integration_vercel", "integration_stripe", "integration_notion", "integration_resend");
  assert.deepEqual(plan("ajoute GitHub", full), { ok: false, reason: "Already 4 pills: remove one first." });
  assert.deepEqual(plan("enlève GitHub", declared("integration_claude")), { ok: false, reason: "GitHub isn't in the island." });
  assert.deepEqual(plan("ajoute GitHub", declared("integration_claude", "integration_github")), {
    ok: false, reason: "GitHub is already in the island.",
  });
});

test("the main pill, replacing and keeping only", () => {
  const main = plan("passe sur Cursor", declared("integration_claude", "agent_cursor", "integration_github"));
  assert.equal(main.ok, true);
  assert.deepEqual(main.next, declared("agent_cursor", "integration_github"));
  const replace = plan("remplace Vercel par GitHub", declared("integration_claude", "integration_vercel"));
  assert.deepEqual(replace.next, declared("integration_claude", "integration_github"));
  const only = plan("garde seulement GitHub", declared("integration_claude", "integration_vercel", "integration_github"));
  assert.deepEqual(only.next, declared("integration_claude", "integration_github"));
});

test("replacing the main pill changes the main and preserves secondary pills", () => {
  const current = declared("integration_claude", "integration_github", "integration_vercel");
  const before = structuredClone(current);
  const got = plan("Replace VS Code with Cursor.", current);
  assert.equal(got.ok, true);
  assert.equal(got.label, "Replace VS Code with Cursor");
  assert.deepEqual(got.next, declared("agent_cursor", "integration_github", "integration_vercel"));
  assert.deepEqual(current, before);
});

test("replacing the main pill works with a full island and removes the new main from secondary pills", () => {
  for (const active of [
    ["integration_github", "integration_vercel", "integration_notion", "integration_stripe"],
    ["integration_github", "agent_cursor", "integration_notion", "integration_stripe"],
  ]) {
    const got = plan("Replace VS Code with Cursor.", declared("integration_claude", ...active));
    assert.equal(got.ok, true);
    assert.deepEqual(got.next, declared("agent_cursor", ...active.filter((id) => id !== "agent_cursor")));
  }
});

test("replacing the main pill with an ineligible main is rejected", () => {
  const current = declared("integration_claude", "integration_vercel");
  assert.deepEqual(plan("Replace VS Code with GitHub.", current), {
    ok: false, reason: "GitHub can't be the main pill.",
  });
  assert.deepEqual(current, declared("integration_claude", "integration_vercel"));
});

test("keeping only too many distinct secondary pills is rejected without changing the current list", () => {
  const current = declared("integration_claude", "integration_n8n");
  const ids = ["integration_github", "integration_vercel", "integration_notion", "integration_stripe", "integration_resend"];
  const got = planPills([{ kind: "pillOnly", ids }], current, "windows");
  assert.deepEqual(got, { ok: false, reason: "Already 4 pills: remove one first." });
  assert.deepEqual(current, declared("integration_claude", "integration_n8n"));
});

test("keeping only deduplicates secondary pills before checking capacity", () => {
  const ids = ["integration_claude", "integration_github", "integration_github", "integration_vercel", "integration_notion", "integration_stripe"];
  const before = [...ids];
  const got = planPills([{ kind: "pillOnly", ids }], declared("integration_claude", "integration_n8n"), "windows");
  assert.equal(got.ok, true);
  assert.equal(got.label, "Keep only VS Code, GitHub, Vercel, Notion, Stripe");
  assert.deepEqual(got.next, declared("integration_claude", "integration_github", "integration_vercel", "integration_notion", "integration_stripe"));
  assert.deepEqual(ids, before);
});

test("anything that isn't only pill commands offers nothing but the chat", () => {
  assert.equal(plan("Hello Mochi, open the chat.", declared("integration_claude")), null);
  assert.equal(plan("pause et ajoute Notion", declared("integration_claude")), null);
  assert.equal(plan("mets du Daft Punk", declared("integration_claude")), null);
});


test("multi-action plans free slots before adding pills", () => {
  const full = declared("integration_claude", "integration_vercel", "integration_stripe", "integration_notion", "integration_resend");
  const got = plan("Add GitHub, then remove Vercel.", full);
  assert.equal(got.ok, true);
  assert.deepEqual(got.next, declared("integration_claude", "integration_stripe", "integration_notion", "integration_resend", "integration_github"));
  assert.deepEqual(full.activeIntegrations, ["integration_vercel", "integration_stripe", "integration_notion", "integration_resend"]);
});

test("a spoken addition can use the slot freed by a later main promotion", () => {
  const current = declared("integration_claude", "agent_cursor", "integration_vercel", "integration_notion", "integration_stripe");
  assert.deepEqual(plan("Add GitHub, then switch to Cursor.", current), {
    ok: true,
    label: "Add GitHub · Make Cursor the main pill",
    next: declared("agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_github"),
  });
});

test("atomic plans check capacity after all requested changes", async (t) => {
  const addGitHub = { kind: "pillAdd", id: "integration_github" };
  const promoteCursor = { kind: "pillSetMain", id: "agent_cursor" };
  const cases = [
    {
      name: "main promotion frees its secondary slot in either order",
      intents: [addGitHub, promoteCursor],
      label: "Add GitHub · Make Cursor the main pill",
      reverseLabel: "Make Cursor the main pill · Add GitHub",
      next: declared("agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_github"),
    },
    {
      name: "main replacement frees its secondary slot",
      intents: [addGitHub, { kind: "pillReplace", old: "integration_claude", new: "agent_cursor" }],
      label: "Add GitHub · Replace VS Code with Cursor",
      reverseLabel: "Replace VS Code with Cursor · Add GitHub",
      next: declared("agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_github"),
    },
    {
      name: "replacement with an active secondary frees a slot",
      intents: [addGitHub, { kind: "pillReplace", old: "integration_vercel", new: "integration_stripe" }],
      label: "Add GitHub · Replace Vercel with Stripe",
      reverseLabel: "Replace Vercel with Stripe · Add GitHub",
      next: declared("integration_claude", "agent_cursor", "integration_notion", "integration_stripe", "integration_github"),
    },
    {
      name: "replacement with the main frees a slot",
      intents: [addGitHub, { kind: "pillReplace", old: "integration_vercel", new: "integration_claude" }],
      label: "Add GitHub · Replace Vercel with VS Code",
      reverseLabel: "Replace Vercel with VS Code · Add GitHub",
      next: declared("integration_claude", "agent_cursor", "integration_notion", "integration_stripe", "integration_github"),
    },
    {
      name: "explicit removals retain their established remove-first order",
      intents: [addGitHub, { kind: "pillRemove", id: "integration_vercel" }],
      label: "Remove Vercel · Add GitHub",
      reverseLabel: "Remove Vercel · Add GitHub",
      next: declared("integration_claude", "agent_cursor", "integration_notion", "integration_stripe", "integration_github"),
    },
    {
      name: "multiple removals free every requested slot in either order",
      intents: [
        { kind: "pillAddMultiple", ids: ["integration_github", "integration_resend"] },
        { kind: "pillRemoveMultiple", ids: ["agent_cursor", "integration_vercel"] },
      ],
      label: "Remove Cursor, Vercel · Add GitHub, Resend",
      reverseLabel: "Remove Cursor, Vercel · Add GitHub, Resend",
      next: declared("integration_claude", "integration_notion", "integration_stripe", "integration_github", "integration_resend"),
    },
    {
      name: "multi-add keeps every new pill after two slots are freed",
      intents: [
        { kind: "pillAddMultiple", ids: ["integration_github", "integration_resend", "integration_github", "integration_claude"] },
        promoteCursor,
        { kind: "pillReplace", old: "integration_vercel", new: "integration_stripe" },
      ],
      label: "Add GitHub, Resend · Make Cursor the main pill · Replace Vercel with Stripe",
      next: declared("agent_cursor", "integration_notion", "integration_stripe", "integration_github", "integration_resend"),
    },
    {
      name: "replacement while temporarily oversized retains its new target",
      intents: [addGitHub, { kind: "pillReplace", old: "integration_vercel", new: "integration_resend" }, promoteCursor],
      label: "Add GitHub · Replace Vercel with Resend · Make Cursor the main pill",
      next: declared("agent_cursor", "integration_notion", "integration_stripe", "integration_github", "integration_resend"),
    },
    {
      name: "replacement can depend on an earlier addition",
      intents: [addGitHub, { kind: "pillReplace", old: "integration_github", new: "integration_resend" }, promoteCursor],
      label: "Add GitHub · Replace GitHub with Resend · Make Cursor the main pill",
      next: declared("agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_resend"),
    },
    {
      name: "keep-only retains every requested pill after a main promotion",
      intents: [
        { kind: "pillOnly", ids: ["agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_github"] },
        promoteCursor,
      ],
      label: "Keep only Cursor, Vercel, Notion, Stripe, GitHub · Make Cursor the main pill",
      next: declared("agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_github"),
    },
    {
      name: "explicit keep-only can reduce a temporarily oversized plan",
      intents: [addGitHub, { kind: "pillOnly", ids: ["integration_github", "integration_notion"] }],
      label: "Add GitHub · Keep only GitHub, Notion",
      next: declared("integration_claude", "integration_github", "integration_notion"),
    },
  ];
  for (const os of ["windows", "linux"]) {
    for (const c of cases) {
      const orders = [[c.intents, c.label]];
      if (c.reverseLabel) orders.push([[...c.intents].reverse(), c.reverseLabel]);
      for (const [intents, label] of orders) {
        await t.test(`${os}: ${c.name}: ${label}`, () => {
          const current = declared("integration_claude", "agent_cursor", "integration_vercel", "integration_notion", "integration_stripe");
          const before = structuredClone({ current, intents });
          assert.deepEqual(planPills(intents, current, os), { ok: true, label, next: c.next });
          assert.deepEqual({ current, intents }, before);
        });
      }
    }
  }
});

test("atomic final-capacity validation rejects overflow and invalid later actions", async (t) => {
  const addGitHub = { kind: "pillAdd", id: "integration_github" };
  const full = "Already 4 pills: remove one first.";
  for (const [name, intents, reason] of [
    ["a new main does not free a secondary slot", [addGitHub, { kind: "pillSetMain", id: "agent_codex" }], full],
    ["one freed slot cannot fit two additions", [
      { kind: "pillAddMultiple", ids: ["integration_github", "integration_resend"] },
      { kind: "pillSetMain", id: "agent_cursor" },
    ], full],
    ["a fresh replacement target does not free a slot", [
      addGitHub, { kind: "pillReplace", old: "integration_vercel", new: "integration_resend" },
    ], full],
    ["an oversized keep-only is not truncated", [
      { kind: "pillOnly", ids: ["agent_cursor", "integration_vercel", "integration_notion", "integration_stripe", "integration_github"] },
      { kind: "pillSetMain", id: "agent_codex" },
    ], full],
    ["replacement still requires its source", [
      addGitHub, { kind: "pillReplace", old: "integration_n8n", new: "integration_stripe" },
    ], "n8n isn't in the island."],
    ["replacement cannot depend on a later addition", [
      { kind: "pillReplace", old: "integration_github", new: "integration_resend" },
      addGitHub, { kind: "pillSetMain", id: "agent_cursor" },
    ], "GitHub isn't in the island."],
    ["a promoted main still needs to be eligible", [addGitHub, { kind: "pillSetMain", id: "integration_github" }], "GitHub can't be the main pill."],
  ]) {
    await t.test(name, () => {
      const current = declared("integration_claude", "agent_cursor", "integration_vercel", "integration_notion", "integration_stripe");
      const before = structuredClone({ current, intents });
      assert.deepEqual(planPills(intents, current, "windows"), { ok: false, reason });
      assert.deepEqual({ current, intents }, before);
    });
  }
});

test("multiple removals run first without mutating the input intent order", () => {
  const intents = [
    { kind: "pillAddMultiple", ids: ["integration_github", "integration_resend"] },
    { kind: "pillRemoveMultiple", ids: ["integration_vercel", "integration_notion"] },
  ];
  const before = structuredClone(intents);
  const full = declared("integration_claude", "integration_vercel", "integration_stripe", "integration_notion", "integration_n8n");
  const got = planPills(intents, full, "windows");
  assert.equal(got.ok, true);
  assert.deepEqual(got.next, declared("integration_claude", "integration_stripe", "integration_n8n", "integration_github", "integration_resend"));
  assert.deepEqual(intents, before);
});

test("a multi-add must fit every distinct new pill or reject the whole plan", () => {
  const current = declared("integration_claude", "integration_vercel", "integration_notion", "integration_stripe");
  const before = structuredClone(current);
  assert.deepEqual(plan("Add GitHub and Resend.", current), {
    ok: false, reason: "Already 4 pills: remove one first.",
  });
  assert.deepEqual(current, before);
});

test("multi-add capacity counts only distinct pills absent from main and secondary slots", () => {
  const current = declared("integration_claude", "integration_vercel", "integration_notion", "integration_stripe");
  const intents = [{ kind: "pillAddMultiple", ids: ["integration_claude", "integration_vercel", "integration_github", "integration_github"] }];
  const before = structuredClone(intents);
  assert.deepEqual(planPills(intents, current, "windows"), {
    ok: true, label: "Add GitHub", next: declared("integration_claude", ...current.activeIntegrations, "integration_github"),
  });
  assert.deepEqual(intents, before);
});

test("a later oversized multi-add cannot leave an earlier partial plan", () => {
  const current = declared("integration_claude", "integration_vercel", "integration_notion");
  const before = structuredClone(current);
  assert.deepEqual(planPills([
    { kind: "pillAdd", id: "integration_stripe" },
    { kind: "pillAddMultiple", ids: ["integration_github", "integration_resend"] },
  ], current, "windows"), { ok: false, reason: "Already 4 pills: remove one first." });
  assert.deepEqual(current, before);
});

test("replacement rejects an absent source instead of adding its target", () => {
  const current = declared("integration_claude", "integration_notion");
  const before = structuredClone(current);
  assert.deepEqual(plan("Replace Vercel with GitHub.", current), {
    ok: false, reason: "Vercel isn't in the island.",
  });
  assert.deepEqual(current, before);
});

test("an absent replacement source is rejected even when its target is already active", () => {
  const current = declared("integration_claude", "integration_github");
  assert.deepEqual(plan("Replace Vercel with GitHub.", current), {
    ok: false, reason: "Vercel isn't in the island.",
  });
});

test("an absent replacement source rejects the whole multi-action plan", () => {
  const current = declared("integration_claude", "integration_notion");
  assert.deepEqual(planPills([
    { kind: "pillAdd", id: "integration_stripe" },
    { kind: "pillReplace", old: "integration_vercel", new: "integration_github" },
  ], current, "windows"), { ok: false, reason: "Vercel isn't in the island." });
  assert.deepEqual(current, declared("integration_claude", "integration_notion"));
});

test("a multi-remove requires every distinct requested pill to be active", async (t) => {
  for (const ids of [
    ["integration_github", "integration_stripe"],
    ["integration_stripe", "integration_github"],
    ["integration_github", "integration_stripe", "integration_stripe"],
    ["integration_github", "integration_claude"],
  ]) {
    await t.test(ids.join(", "), () => {
      const current = declared("integration_claude", "integration_github", "integration_vercel");
      const intents = [{ kind: "pillRemoveMultiple", ids }];
      const before = structuredClone({ current, intents });
      const missing = ids.includes("integration_stripe") ? "Stripe" : "VS Code";
      assert.deepEqual(planPills(intents, current, "windows"), {
        ok: false, reason: `${missing} isn't in the island.`,
      });
      assert.deepEqual({ current, intents }, before);
    });
  }
});

test("multi-remove deduplicates a fully active request and preserves the input", () => {
  const current = declared("integration_claude", "integration_github", "integration_vercel", "integration_stripe");
  const intents = [{ kind: "pillRemoveMultiple", ids: ["integration_github", "integration_stripe", "integration_github"] }];
  const before = structuredClone({ current, intents });
  assert.deepEqual(planPills(intents, current, "windows"), {
    ok: true, label: "Remove GitHub, Stripe", next: declared("integration_claude", "integration_vercel"),
  });
  assert.deepEqual({ current, intents }, before);
});

test("a missing multi-remove target rejects earlier removals and later additions", () => {
  const current = declared("integration_claude", "integration_github", "integration_vercel");
  const intents = [
    { kind: "pillAdd", id: "integration_notion" },
    { kind: "pillRemove", id: "integration_vercel" },
    { kind: "pillRemoveMultiple", ids: ["integration_github", "integration_stripe"] },
  ];
  const before = structuredClone({ current, intents });
  assert.deepEqual(planPills(intents, current, "windows"), {
    ok: false, reason: "Stripe isn't in the island.",
  });
  assert.deepEqual({ current, intents }, before);
});

test("explicit pill lists reject every unresolved nonempty segment", async (t) => {
  for (const trigger of ["Keep only", "Garde seulement", "Add", "Ajoute", "Remove", "Enlève"]) {
    for (const list of [
      "GitHub and Frobnitz",
      "Frobnitz and GitHub",
      "GitHub and Frobnitz and Stripe",
      "GitHub et Stripe et Frobnitz",
      "the GitHub and the pill",
    ]) {
      const text = `${trigger} ${list}.`;
      await t.test(text, () => {
        const current = declared("integration_claude", "integration_github", "integration_vercel", "integration_stripe");
        const before = structuredClone(current);
        assert.deepEqual(parseIntent(text, availablePills("windows")), { kind: "unknown" });
        assert.deepEqual(parseTranscript(text, availablePills("windows")), []);
        assert.equal(plan(text, current), null);
        assert.deepEqual(current, before);
      });
    }
  }
});

test("ambiguous music or pill lists never offer just the resolved subset", async (t) => {
  for (const trigger of ["Mets", "Play", "Lance"]) {
    const text = `${trigger} GitHub et Frobnitz et Stripe.`;
    await t.test(text, () => {
      const intents = parseTranscript(text, availablePills("windows"));
      assert.ok(intents.every((intent) => !intent.kind.startsWith("pill")));
      assert.equal(planPills(intents, declared("integration_claude"), "windows"), null);
    });
  }
});

test("fully resolved pill lists preserve aliases, articles, order and duplicates", async (t) => {
  for (const [trigger, kind] of [
    ["Keep only", "pillOnly"], ["Garde seulement", "pillOnly"],
    ["Add", "pillAddMultiple"], ["Remove", "pillRemoveMultiple"], ["Mets", "pillAddMultiple"],
  ]) {
    await t.test(trigger, () => {
      assert.deepEqual(parseTranscript(`${trigger} the GT and la Stripe and GitHub.`, availablePills("windows")), [
        { kind, ids: ["integration_github", "integration_stripe", "integration_github"] },
      ]);
    });
  }
});

test("keep-only accepts supported polite prefixes without dropping the first pill", () => {
  for (const prefix of ["Please keep only", "Can you keep only", "Est-ce que tu peux garder seulement"]) {
    assert.deepEqual(parseTranscript(`${prefix} GitHub and Stripe.`, availablePills("windows")), [
      { kind: "pillOnly", ids: ["integration_github", "integration_stripe"] },
    ]);
  }
});

test("incidental or negated keep-only mentions are not destructive commands", () => {
  for (const prefix of [
    "Please explain how to keep only", "I do not want to keep only", "I want to keep only",
  ]) {
    for (const list of ["GitHub", "GitHub and Stripe"]) {
      assert.deepEqual(parseTranscript(`${prefix} ${list}.`, availablePills("windows")), []);
    }
  }
});

test("one unresolved keep-only list rejects an entire multi-action transcript", async (t) => {
  for (const text of [
    "Keep only GitHub and Frobnitz, then remove Vercel.",
    "Remove Vercel, then keep only GitHub and Frobnitz.",
    "Keep only GitHub and Frobnitz, then add Notion.",
  ]) {
    await t.test(text, () => {
      assert.deepEqual(parseTranscript(text, availablePills("windows")), []);
    });
  }
});

test("failed multi-action parsing cannot fall back to one surviving pill command", async (t) => {
  for (const text of [
    "Add GitHub and Frobnitz and Stripe, then remove Vercel.",
    "Add GitHub and Frobnitz and Stripe then remove Vercel.",
    "Ajoute GitHub et Frobnitz et Stripe, puis enlève Vercel.",
    "Remove Vercel, then add GitHub and Frobnitz and Stripe.",
    "Keep only GitHub and Frobnitz, then replace Vercel with Notion.",
    "Remove GitHub and Frobnitz then switch to Cursor.",
    "Add GitHub and Frobnitz, then keep only Notion.",
    "Replace Frobnitz with GitHub and add Stripe.",
    "Keep only GitHub and Frobnitz puis mets la principale sur Cursor.",
    "Je veux GitHub et Frobnitz, puis retire Vercel.",
    "GitHub instead of Frobnitz, then add Stripe.",
    "GitHub à la place de Frobnitz, puis ajoute Stripe.",
    "Keep only GitHub and Frobnitz then replace Vercel with Notion, then add Stripe.",
    "Add GitHub and Frobnitz and affiche Stripe, then remove Vercel.",
    "Remove GitHub and Frobnitz then switch to Cursor, then add Stripe.",
    "Keep only Frobnitz replace Vercel with Notion and add Stripe.",
    "Replace Frobnitz with GitHub switch to Cursor and remove Vercel.",
    "Keep only GitHub and Frobnitz replace Vercel with Notion and add Stripe.",
    "Keep only GitHub and Frobnitz remove Vercel.",
    "Keep only GitHub and Frobnitz, then la principale c'est Cursor.",
    "Add GitHub and Frobnitz then la principale pour Cursor.",
    "GitHub instead of Frobnitz, puis la principale c'est Cursor.",
    "Keep only GitHub and Frobnitz then la principale c'est Cursor, then add Stripe.",
    "Keep only GitHub and Frobnitz la principale c'est Cursor.",
    "La principale c'est Frobnitz, puis ajoute GitHub.",
    "La pilule principale c'est Frobnitz et ajoute GitHub.",
    "Frobnitz comme pilule principale, puis ajoute GitHub.",
    "La principale c'est Frobnitz, then la principale pour Cursor.",
    "Frobnitz comme pilule principale, then la principale c'est Cursor.",
    "Switch to Frobnitz la principale c'est Cursor.",
    "Switch to Frobnitz la principale c'est Cursor, then add Resend.",
    "Switch to Frobnitz la principale pour Cursor.",
    "Switch to Frobnitz la principale pour Cursor, then add Resend.",
  ]) {
    await t.test(text, () => {
      assert.equal(parseMultiAction(text, availablePills("windows")), null);
      assert.deepEqual(parseTranscript(text, availablePills("windows")), []);
    });
  }
});

test("single-command fallback preserves punctuation and supported politeness", () => {
  const pills = availablePills("windows");
  assert.deepEqual(parseTranscript("Ajoute GitHub, s'il te plaît.", pills), [{ kind: "pillAdd", id: "integration_github" }]);
  assert.deepEqual(parseTranscript("Je veux que tu ajoutes GitHub.", pills), [{ kind: "pillAdd", id: "integration_github" }]);
  assert.deepEqual(parseTranscript("Keep only GitHub, and Stripe.", pills), [{
    kind: "pillOnly", ids: ["integration_github", "integration_stripe"],
  }]);
  assert.deepEqual(parseTranscript("Play Make You Feel My Love.", pills), [{
    kind: "musicPlaySearch", name: "Make You Feel My Love",
  }]);
  for (const text of ["GitHub instead of Vercel", "Add GitHub instead of Vercel"]) {
    assert.deepEqual(parseTranscript(text, pills), [{
      kind: "pillReplace", old: "integration_vercel", new: "integration_github",
    }]);
  }
  assert.deepEqual(parseTranscript("GitHub instead of Vercel, then add Stripe.", pills), [
    { kind: "pillReplace", old: "integration_vercel", new: "integration_github" },
    { kind: "pillAdd", id: "integration_stripe" },
  ]);
  for (const text of ["Mets Cursor en principale", "Mets la principale sur Cursor", "La principale c'est Cursor", "Cursor comme pilule principale"]) {
    assert.deepEqual(parseTranscript(text, pills), [{ kind: "pillSetMain", id: "agent_cursor" }]);
  }
});

test("every pill operation rejects unknown or unavailable IDs before planning", async (t) => {
  for (const [os, id] of [
    ["windows", "missing"], ["windows", ""], ["windows", "integration_spotify"],
    ["linux", "agent_claude-desktop"],
  ]) {
    for (const intent of [
      { kind: "pillAdd", id },
      { kind: "pillAddMultiple", ids: ["integration_notion", id] },
      { kind: "pillRemove", id },
      { kind: "pillRemoveMultiple", ids: ["integration_github", id] },
      { kind: "pillSetMain", id },
      { kind: "pillReplace", old: "integration_github", new: id },
      { kind: "pillReplace", old: id, new: "integration_notion" },
      { kind: "pillOnly", ids: ["integration_github", id] },
    ]) {
      await t.test(`${os}: ${JSON.stringify(intent)}`, () => {
        const current = declared("integration_claude", "integration_github", "integration_vercel");
        const intents = [{ kind: "pillAdd", id: "integration_stripe" }, intent];
        const before = structuredClone({ current, intents });
        const got = planPills(intents, current, os);
        assert.equal(got?.ok, false);
        assert.equal("next" in got, false);
        assert.deepEqual({ current, intents }, before);
      });
    }
  }
});

test("empty pill lists cannot clear the island or leave an earlier partial plan", async (t) => {
  for (const kind of ["pillOnly", "pillAddMultiple", "pillRemoveMultiple"]) {
    await t.test(kind, () => {
      const current = declared("integration_claude", "integration_github", "integration_vercel");
      const intents = [{ kind: "pillAdd", id: "integration_notion" }, { kind, ids: [] }];
      const before = structuredClone({ current, intents });
      assert.equal(planPills(intents, current, "windows")?.ok, false);
      assert.deepEqual({ current, intents }, before);
    });
  }
});

test("empty conjunction segments are ignored but a list still needs a resolved pill", () => {
  const pills = availablePills("windows");
  assert.deepEqual(parseTranscript("Keep only and GitHub and and Stripe and.", pills), [
    { kind: "pillOnly", ids: ["integration_github", "integration_stripe"] },
  ]);
  assert.deepEqual(parseTranscript("Keep only and and.", pills), []);
  assert.deepEqual(parseTranscript("Keep only the pill.", pills), []);
});

test("keep-only labels name an explicitly retained main pill", () => {
  const current = declared("integration_claude", "integration_github", "integration_vercel");
  for (const [ids, label, active] of [
    [["integration_claude"], "Keep only VS Code", []],
    [["integration_claude", "integration_github", "integration_claude"], "Keep only VS Code, GitHub", ["integration_github"]],
  ]) {
    assert.deepEqual(planPills([{ kind: "pillOnly", ids }], current, "windows"), {
      ok: true, label, next: declared("integration_claude", ...active),
    });
  }
  assert.deepEqual(current, declared("integration_claude", "integration_github", "integration_vercel"));
});
