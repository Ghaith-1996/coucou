// The intent parser (src/assistant/intents.ts) against the Mac's own table,
// tests/IntentParserTests.swift, converted case for case into
// fixtures/mac-intent-cases.json. Like the Mac's PillFixture, the pills are
// the whole catalog.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { appToOpen, normalise, parseIntent, parseMultiAction, parseTranscript, resolvePill } from "../src/assistant/intents.ts";
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
  assert.equal(got.label, "Keep only GitHub, Vercel, Notion, Stripe");
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

test("open an app: the Mac's VoiceQueryTests cases, then Whisper's", () => {
  assert.equal(appToOpen("ouvre Figma"), "Figma");
  assert.equal(appToOpen("ouvre l'app Notes"), "Notes");
  assert.equal(appToOpen("open Safari"), "Safari");
  assert.deepEqual(parseTranscript("ouvre Figma", PILL_CATALOG), [{ kind: "openApp", name: "Figma" }]);
  // Whisper's punctuation and capitals; the name kept as said.
  assert.deepEqual(parseTranscript("Ouvre Visual Studio Code.", PILL_CATALOG), [{ kind: "openApp", name: "Visual Studio Code" }]);
  assert.equal(appToOpen("Peux-tu ouvrir Spotify ?"), "Spotify");
  assert.equal(appToOpen("Open the app Notion"), "Notion");
  assert.equal(appToOpen("ouvre-moi la calculatrice"), "calculatrice");
  // Nothing to open, or a sentence being dictated.
  assert.equal(appToOpen("ouvre"), null);
  assert.equal(appToOpen("open the app"), null);
  assert.equal(appToOpen("please open the chat and show me the latest pull request"), null);
  assert.equal(appToOpen("Ajoute GitHub"), null);
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
