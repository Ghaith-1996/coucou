// Audio mode's reading of replies (src/assistant/speakable.ts).

import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_SPEECH_CHARS, MIN_CHUNK, nextChunk, speakable, speechChunks } from "../src/assistant/speakable.ts";

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

function boundedSpeech(text) {
  const chunks = speechChunks(text);
  assert.equal(chunks.join(""), text, "splitting preserves every character in order");
  for (const chunk of chunks) {
    assert.ok(chunk.length > 0);
    assert.ok([...chunk].length <= MAX_SPEECH_CHARS, "the cap counts Unicode scalars like Rust chars()");
    assert.equal(chunk.isWellFormed(), true, "surrogate pairs stay intact");
  }
  return chunks;
}

test("the speech cap matches both native provider truncation paths", async () => {
  const { readFileSync } = await import("node:fs");
  const source = readFileSync(new URL("../src-tauri/src/assistant/speech.rs", import.meta.url), "utf8");
  const nativeCap = Number(source.match(/pub const MAX_CHARS: usize = (\d+);/)[1]);
  assert.equal(MAX_SPEECH_CHARS, nativeCap);
  assert.equal((source.match(/text\.chars\(\)\.take\(MAX_CHARS\)/g) ?? []).length, 2,
    "Windows synthesis and ChatGPT dispatch have the same scalar cap");
});

test("speech splitting accepts exact-cap ASCII and astral text without an extra clip", () => {
  assert.deepEqual(speechChunks(""), []);
  for (const text of ["a".repeat(1500), "a".repeat(1499) + "😀", "😀".repeat(1500)]) {
    assert.deepEqual(boundedSpeech(text), [text]);
  }
  assert.deepEqual(boundedSpeech("😀".repeat(1501)).map(chunk => [...chunk].length), [1500, 1]);
});

test("oversized speech prefers whitespace and punctuation while keeping all text", () => {
  const words = "A useful phrase, ".repeat(240) + "the final words.";
  const chunks = boundedSpeech(words);
  assert.ok(chunks.length > 1);
  for (const chunk of chunks.slice(0, -1)) assert.match(chunk, /[ ,]$/);
  const chinese = "一段没有空格的说明，".repeat(350) + "最后一句。";
  const cjk = boundedSpeech(chinese);
  for (const chunk of cjk.slice(0, -1)) assert.match(chunk, /，$/);
});

test("long unbroken words preserve surrogate pairs, combining sequences and emoji clusters", () => {
  for (const grapheme of ["😀", "e\u0301", "👩🏽‍💻", "👨‍👩‍👧‍👦"]) {
    const chunks = boundedSpeech(grapheme.repeat(1700));
    for (const chunk of chunks) assert.equal(chunk, grapheme.repeat(chunk.length / grapheme.length));
  }
  boundedSpeech("a" + "\u0301".repeat(3000)); // even one oversized grapheme cannot exceed the provider cap
});

test("oversized Markdown speech is split after stripping links, emphasis and code", () => {
  const label = "important word ".repeat(300).trim();
  const markdown = `Read **[${label}](https://private.example/hidden)** then finish.\n\n\`\`\`js\nsecret code\n\`\`\``;
  const chunk = nextChunk(markdown, 0, true);
  assert.ok([...chunk.text].length > MAX_SPEECH_CHARS);
  const clips = boundedSpeech(chunk.text);
  assert.equal(clips.join(""), `Read ${label} then finish.`);
  assert.equal(nextChunk(markdown, chunk.end, true), null);
});

test("older webviews without grapheme segmentation still bound Unicode speech safely", async () => {
  const original = Intl.Segmenter;
  try {
    Intl.Segmenter = undefined;
    const fallback = await import(new URL("../src/assistant/speakable.ts?without-segmenter", import.meta.url).href);
    const text = "😀".repeat(3500);
    const chunks = fallback.speechChunks(text);
    assert.equal(chunks.join(""), text);
    assert.deepEqual(chunks.map(chunk => [...chunk].length), [1500, 1500, 500]);
    assert.ok(chunks.every(chunk => chunk.isWellFormed()));
  } finally {
    Intl.Segmenter = original;
  }
});
