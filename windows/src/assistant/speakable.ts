// What audio mode reads aloud: a reply's markdown as plain sentences, and the
// next complete chunk of a reply that is still streaming in, so the first
// sentence is heard while the rest is being written.

/** Markdown → text a voice can read: no code, links by their words, no markup. */
export function speakable(markdown: string): string {
  return markdown
    .replace(/```[\s\S]*?(```|$)/g, " ")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, " ")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, " ")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/^\s{0,3}#{1,6}\s+/gm, "")
    .replace(/^\s{0,3}>\s?/gm, "")
    .replace(/^\s*(?:[-*+]|\d+[.)])\s+/gm, "")
    .replace(/^\s*\|?[\s:-]+\|[\s|:-]*$/gm, " ")
    .replace(/\|/g, ", ")
    .replace(/(\*\*|__|\*|_|~~)(?=\S)([^*_~]+?)(?<=\S)\1/g, "$2")
    .replace(/[*_~#]{2,}/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

/** Both providers in assistant/speech.rs cap a clip at this many Unicode scalars. */
export const MAX_SPEECH_CHARS = 1500;
// Older non-Windows webviews still import this module even without audio mode.
const speechSegments = typeof Intl.Segmenter === "function"
  ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;

/**
 * Split cleaned speech, not raw markdown: splitting markup could expose a link
 * target or fenced code. Preserve every character, preferring word/punctuation
 * boundaries and keeping graphemes whole whenever they fit the provider cap.
 */
export function speechChunks(text: string): string[] {
  if (!text) return [];
  if ([...text].length <= MAX_SPEECH_CHARS) return [text];
  const segments = speechSegments ? [...speechSegments.segment(text)].map(({ segment }) => segment) : [...text];
  const units = segments.flatMap(segment => {
    const points = [...segment];
    // A pathological grapheme (e.g. thousands of combining marks) must still
    // respect the native scalar limit; split only that unit by code point.
    return points.length > MAX_SPEECH_CHARS
      ? points.map(text => ({ text, size: 1 }))
      : [{ text: segment, size: points.length }];
  });
  const chunks: string[] = [];
  for (let start = 0; start < units.length;) {
    let end = start;
    let size = 0;
    let boundary = start;
    while (end < units.length && size + units[end].size <= MAX_SPEECH_CHARS) {
      size += units[end].size;
      if (/[\s,;:!?…。！？、，；：]$/u.test(units[end].text)) boundary = end + 1;
      end++;
    }
    const cut = end < units.length && boundary > start ? boundary : end;
    chunks.push(units.slice(start, cut).map(unit => unit.text).join(""));
    start = cut;
  }
  return chunks;
}

/** The shortest chunk worth a clip of its own (except the last). */
export const MIN_CHUNK = 24;

/**
 * The next chunk of `full` (raw markdown) after `from`: whole sentences up to
 * a boundary, at least MIN_CHUNK long once readable. `done` means the reply
 * is complete, so whatever is left is the last chunk. Null when nothing is
 * ready yet. `end` is where the next call starts.
 */
export function nextChunk(full: string, from: number, done: boolean): { text: string; end: number } | null {
  const rest = full.slice(from);
  if (!rest.trim()) return null;
  const boundary = /([.!?…:;])(?=\s)|\n\s*\n|\n(?=\s*(?:[-*+]|\d+[.)])\s)/g;
  let end = -1;
  for (let m = boundary.exec(rest); m; m = boundary.exec(rest)) {
    const cut = m.index + m[0].length;
    const before = rest.slice(0, cut);
    // Never cut inside a code block that is still open.
    if ((before.match(/```/g) ?? []).length % 2 === 1) continue;
    if (speakable(before).length >= MIN_CHUNK) {
      end = cut;
      break;
    }
  }
  if (end < 0) {
    if (!done) return null;
    end = rest.length;
  }
  const text = speakable(rest.slice(0, end));
  if (!text) return end === rest.length ? null : nextChunk(full, from + end, done);
  return { text, end: from + end };
}
