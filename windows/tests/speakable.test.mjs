// Audio mode's reading of replies (src/assistant/speakable.ts).

import { test } from "node:test";
import assert from "node:assert/strict";
import { MIN_CHUNK, nextChunk, speakable } from "../src/assistant/speakable.ts";

test("markup is read as words, code and addresses are skipped", () => {
  assert.equal(speakable("## Steps\n- Click **Settings**\n- Open [the docs](https://x.y/z)"), "Steps Click Settings Open the docs");
  assert.equal(speakable("Run `npm test`:\n```bash\nnpm test\n```\nDone."), "Run npm test: Done.");
  assert.equal(speakable("See https://example.com/a?b=c for more."), "See for more.");
  assert.equal(speakable("> quoted _text_ and ~~gone~~"), "quoted text and gone");
  assert.equal(speakable("![logo](a.png) Hi"), "Hi");
});

test("a streaming reply is read sentence by sentence", () => {
  const full = "Bonjour ! Voici la réponse à ta question. Et une deuxième phrase";
  const first = nextChunk(full, 0, false);
  assert.deepEqual(first, { text: "Bonjour ! Voici la réponse à ta question.", end: 41 });
  assert.equal(nextChunk(full, first.end, false), null, "the last sentence isn't finished");
  assert.deepEqual(nextChunk(full, first.end, true), { text: "Et une deuxième phrase", end: full.length });
  assert.equal(nextChunk(full, full.length, true), null);
});

test("short sentences are joined until the chunk is worth a clip", () => {
  const full = "Yes. Sure. That works well for most setups. Next";
  const chunk = nextChunk(full, 0, false);
  assert.ok(chunk.text.length >= MIN_CHUNK);
  assert.equal(chunk.text, "Yes. Sure. That works well for most setups.");
});

test("an open code block is never cut, and is skipped once closed", () => {
  const open = "Here is the fix:\n```js\nconst a = 1; const b = 2;\n";
  const first = nextChunk(open, 0, false);
  assert.equal(first, null, "too short before the fence, and the fence is open");
  const closed = open + "```\nThat should do it, try it now.";
  const chunk = nextChunk(closed, 0, true);
  assert.equal(chunk.text, "Here is the fix: That should do it, try it now.");
});

test("list items and paragraphs are boundaries", () => {
  const full = "Three things to check today:\n- the cable is plugged in\n- the driver is current\n";
  const a = nextChunk(full, 0, false);
  assert.equal(a.text, "Three things to check today:");
  const b = nextChunk(full, a.end, true);
  assert.equal(b.text, "the cable is plugged in the driver is current");
});
