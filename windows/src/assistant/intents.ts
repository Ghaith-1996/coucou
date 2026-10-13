// What a transcript asks for — port of IntentParser.swift and
// EntityResolver.swift (NotchBuddy/Sources/App/Voice), French and English,
// deterministic, no model. tests/assistant-intents.test.mjs carries the Mac's
// IntentParserTests table.
//
// What can be acted on in this version: the pill commands (pills.ts → a
// `pill-set` action), and "open <app>" (VoiceQuery.appToOpen → a `launch-app`
// action, after the user confirms the app Rust found in the Start menu).
// Music is recognised as on the Mac so that ambiguous verbs ("lance
// Spotify", "mets du Daft Punk") resolve the same way, but there is no Windows
// media adapter yet: a music intent offers nothing. The Mac's mail, questions
// and web search (VoiceQuery) are not ported: those sentences are simply not
// commands here.

import type { PillCategory, PillDefinition } from "../core/pills";

export type Intent =
  | { kind: "pillAdd"; id: string }
  | { kind: "pillAddMultiple"; ids: string[] }
  | { kind: "pillRemove"; id: string }
  | { kind: "pillRemoveMultiple"; ids: string[] }
  | { kind: "pillSetMain"; id: string }
  | { kind: "pillReplace"; old: string; new: string }
  | { kind: "pillOnly"; ids: string[] }
  | { kind: "musicPlay"; target: "appleMusic" | "spotify" | null }
  | { kind: "musicPause" }
  | { kind: "musicNext" }
  | { kind: "musicPrevious" }
  | { kind: "musicVolumeUp" }
  | { kind: "musicVolumeDown" }
  | { kind: "musicSetVolume"; percent: number }
  | { kind: "musicPlaySearch"; name: string }
  | { kind: "musicPlayPlaylist"; name: string }
  | { kind: "openApp"; name: string }
  | { kind: "unknown" };

const UNKNOWN: Intent = { kind: "unknown" };

// ── Normalisation ─────────────────────────────────────────────────────────────

export function normalise(s: string): string {
  return s
    .toLowerCase()
    .replace(/['’-]/g, " ")
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .replace(/[^\p{L}\p{N} ]/gu, "")
    .split(" ")
    .filter(Boolean)
    .join(" ");
}

/** Original casing and accents, split like `normalise`, pure punctuation dropped. */
function buildRawWords(raw: string): string[] {
  return raw
    .replace(/['’-]/g, " ")
    .split(" ")
    .filter((w) => w !== "" && /[\p{L}\p{N}]/u.test(w));
}

const FILLERS = new Set([
  "un", "peu", "s", "il", "te", "plait", "moi", "truc", "chose", "machin", "bidule", "pile", "piles", "aussi", "stp",
]);
const POSSESSIVES: Record<string, string> = { ma: "la", mon: "le", mes: "les" };

function defilter(words: string[], rawWords: string[]): [string[], string[]] {
  const fw: string[] = [];
  const fr: string[] = [];
  const n = Math.min(words.length, rawWords.length);
  for (let i = 0; i < n; i++) {
    const w = words[i];
    if (POSSESSIVES[w]) {
      fw.push(POSSESSIVES[w]);
      fr.push(rawWords[i]);
    } else if (!FILLERS.has(w)) {
      fw.push(w);
      fr.push(rawWords[i]);
    }
  }
  return [fw, fr];
}

// ── Sequence helpers ──────────────────────────────────────────────────────────

function indexOfSequence(seq: string[], words: string[]): number {
  if (seq.length === 0 || seq.length > words.length) return -1;
  for (let i = 0; i <= words.length - seq.length; i++) {
    if (seq.every((s, k) => words[i + k] === s)) return i;
  }
  return -1;
}

const containsSeq = (words: string[], seq: string[]) => indexOfSequence(seq, words) >= 0;
const matchesAny = (words: string[], table: string[][]) => table.some((seq) => containsSeq(words, seq));

/** Tail after the first matching trigger, longest triggers first. */
function extractAfter(words: string[], rawWords: string[], triggers: string[][]): { norm: string; raw: string } | null {
  const sorted = [...triggers].sort((a, b) => b.length - a.length);
  for (const trigger of sorted) {
    if (trigger.length >= words.length) continue;
    for (let i = 0; i <= words.length - trigger.length; i++) {
      if (trigger.every((t, k) => words[i + k] === t)) {
        const norm = words.slice(i + trigger.length).join(" ");
        const raw = i + trigger.length < rawWords.length ? rawWords.slice(i + trigger.length).join(" ") : "";
        if (norm) return { norm, raw };
      }
    }
  }
  return null;
}

const ARTICLES = new Set([
  "du", "de", "la", "le", "les", "des", "l", "some", "the", "a", "an", "pilule", "pill", "pile", "piles", "ma", "mon", "mes",
]);

function stripArticles(name: string): string {
  const ws = name.split(" ").filter(Boolean);
  while (ws.length && ARTICLES.has(ws[0])) ws.shift();
  return ws.join(" ");
}

function stripArticlesRaw(name: string): string {
  const ws = name.split(" ").filter(Boolean);
  while (ws.length && ARTICLES.has(ws[0].toLowerCase())) ws.shift();
  return ws.join(" ");
}

function splitByConjunction(name: string): string[] {
  const parts: string[][] = [];
  let cur: string[] = [];
  for (const w of name.split(" ").filter(Boolean)) {
    if (w === "et" || w === "and") {
      if (cur.length) parts.push(cur);
      cur = [];
    } else cur.push(w);
  }
  if (cur.length) parts.push(cur);
  return parts.map((p) => p.join(" "));
}

function stripTrailingInstead(words: string[], rawWords: string[]): [string[], string[]] {
  for (const t of [["a", "la", "place"], ["a", "sa", "place"], ["instead"]]) {
    if (words.length > t.length && t.every((x, k) => words[words.length - t.length + k] === x)) {
      const n = words.length - t.length;
      return [words.slice(0, n), rawWords.slice(0, Math.min(n, rawWords.length))];
    }
  }
  return [words, rawWords];
}

function stripTrailingLocation(words: string[], rawWords: string[]): [string[], string[]] {
  const i = words.lastIndexOf("dans");
  if (i <= 0) return [words, rawWords];
  const tail = words.slice(i + 1);
  const articles = new Set(["les", "la", "le", "l"]);
  const places = new Set(["pilules", "pilule", "notch", "encoche", "barre", "liste"]);
  const ok = (tail.length === 1 && articles.has(tail[0]))
    || (tail.length === 2 && articles.has(tail[0]) && places.has(tail[1]));
  if (!ok) return [words, rawWords];
  return [words.slice(0, i), rawWords.slice(0, Math.min(i, rawWords.length))];
}

// ── Entities (EntityResolver.swift) ───────────────────────────────────────────

export const ALIASES: Record<string, string> = {
  "claude": "integration_claude",
  "claude code": "integration_claude",
  "vs code": "integration_claude",
  "vscode": "integration_claude",
  "v s code": "integration_claude",
  "visual studio code": "integration_claude",
  "visual studio": "integration_claude",
  "ves code": "integration_claude",
  "vee s code": "integration_claude",
  "vi s code": "integration_claude",
  "vsco": "integration_claude",
  "gemini": "agent_gemini",
  "copilot": "agent_copilot",
  "muse": "agent_muse",
  "hermes": "agent_hermes",
  "amp": "agent_amp",
  "opencode": "agent_opencode",
  "antigravity": "agent_antigravity",
  "codex": "agent_codex",
  "claude desktop": "agent_claude-desktop",
  "google": "ai_google",
  "openai": "ai_openai",
  "open ai": "ai_openai",
  "ollama": "ai_ollama",
  "lmstudio": "ai_lmstudio",
  "anthropic": "ai_anthropic",
  "github": "integration_github",
  "gt": "integration_github",
  "g t": "integration_github",
  "git hub": "integration_github",
  "jit hub": "integration_github",
  "guitare hub": "integration_github",
  "guitar hub": "integration_github",
  "gitub": "integration_github",
  "guit hub": "integration_github",
  "vercel": "integration_vercel",
  "notion": "integration_notion",
  "stripe": "integration_stripe",
  "resend": "integration_resend",
  "n8n": "integration_n8n",
  "calcom": "integration_calcom",
  "cal": "integration_calcom",
  "spotify": "integration_spotify",
  "apple music": "integration_music",
  "music": "integration_music",
  "musique": "integration_music",
};

export function levenshtein(a: string, b: string): number {
  const ac = [...a];
  const bc = [...b];
  if (ac.length === 0) return bc.length;
  if (bc.length === 0) return ac.length;
  const row = Array.from({ length: bc.length + 1 }, (_, j) => j);
  for (let i = 1; i <= ac.length; i++) {
    let prev = row[0];
    row[0] = i;
    for (let j = 1; j <= bc.length; j++) {
      const temp = row[j];
      row[j] = ac[i - 1] === bc[j - 1] ? prev : Math.min(prev, row[j], row[j - 1]) + 1;
      prev = temp;
    }
  }
  return row[bc.length];
}

/** Aliases → exact name → whole word in the name → Levenshtein ≤ 2 (≤ 1 for short
 *  queries). Null when nothing is close enough, or when two are. */
export function resolvePill(query: string, pills: readonly PillDefinition[], category?: PillCategory): string | null {
  const q = normalise(query);
  const pool = category ? pills.filter((p) => p.category === category) : pills;
  if (pool.length === 0 || !q) return null;

  const alias = ALIASES[q];
  if (alias && pool.some((p) => p.id === alias)) return alias;

  const exact = pool.find((p) => normalise(p.name) === q);
  if (exact) return exact.id;

  const wordMatches = pool.filter((p) => normalise(p.name).split(" ").includes(q));
  if (wordMatches.length === 1) return wordMatches[0].id;
  if (wordMatches.length > 1) return null;

  let best: { id: string; dist: number } | null = null;
  let second: { id: string; dist: number } | null = null;
  for (const p of pool) {
    const d = levenshtein(q, normalise(p.name));
    if (d < (best?.dist ?? Infinity)) {
      second = best;
      best = { id: p.id, dist: d };
    } else if (d < (second?.dist ?? Infinity)) {
      second = { id: p.id, dist: d };
    }
  }
  if (!best) return null;
  const tolerance = [...q].length <= 4 ? 1 : 2;
  if (best.dist > tolerance) return null;
  if (second && second.dist <= tolerance) return null;
  return best.id;
}

// ── Pill extraction ───────────────────────────────────────────────────────────

function extractReplace(words: string[], pills: readonly PillDefinition[]): [string, string] | null {
  for (const start of [["remplace"], ["replace"], ["echange"], ["swap"], ["change"]]) {
    const startIdx = indexOfSequence(start, words);
    if (startIdx < 0) continue;
    const rest = words.slice(startIdx + start.length);
    for (const sep of ["par", "for", "contre", "with", "by"]) {
      const sepIdx = rest.indexOf(sep);
      if (sepIdx <= 0 || sepIdx >= rest.length - 1) continue;
      const id1 = resolvePill(stripArticles(rest.slice(0, sepIdx).join(" ")), pills);
      const id2 = resolvePill(stripArticles(rest.slice(sepIdx + 1).join(" ")), pills);
      if (id1 && id2) return [id1, id2];
    }
  }
  return null;
}

/** "mets X à la place de Y" / "X au lieu de Y" / "X instead of Y" → [Y, X]. */
function extractInsteadOf(words: string[], pills: readonly PillDefinition[]): [string, string] | null {
  const markers = [["a", "la", "place", "de"], ["a", "la", "place", "du"], ["au", "lieu", "de"], ["au", "lieu", "du"], ["instead", "of"]];
  const leadVerbs = new Set(["mets", "met", "remets", "ajoute", "rajoute", "active", "affiche", "put", "add", "use", "utilise", "prends"]);
  const noise = new Set(["la", "le", "les", "l", "pilule", "pilules", "the", "pill", "me", "moi"]);
  for (const m of markers) {
    const i = indexOfSequence(m, words);
    if (i <= 0 || i + m.length >= words.length) continue;
    const left = words.slice(0, i).filter((w) => !leadVerbs.has(w) && !noise.has(w));
    const right = words.slice(i + m.length).filter((w) => !noise.has(w));
    if (!left.length || !right.length) continue;
    const added = resolvePill(left.join(" "), pills);
    const removed = resolvePill(right.join(" "), pills);
    if (!added || !removed || added === removed) continue;
    return [removed, added];
  }
  return null;
}

function extractOnly(words: string[], pills: readonly PillDefinition[]): string[] | null {
  for (const start of [["garde", "seulement"], ["keep", "only"], ["garder", "seulement"]]) {
    if (!containsSeq(words, start)) continue;
    const ids: string[] = [];
    for (const part of splitByConjunction(words.slice(start.length).join(" "))) {
      const clean = stripArticles(part);
      const id = clean ? resolvePill(clean, pills) : null;
      if (id) ids.push(id);
    }
    if (ids.length) return ids;
  }
  return null;
}

function resolveAll(clean: string, pills: readonly PillDefinition[]): string[] {
  return splitByConjunction(clean)
    .map((part) => stripArticles(part))
    .filter(Boolean)
    .map((p) => resolvePill(p, pills))
    .filter((id): id is string => id != null);
}

function extractVolume(words: string[]): number | null {
  const volIdx = words.indexOf("volume");
  if (volIdx < 0) return null;
  let numIdx = -1;
  for (let i = words.length - 1; i >= 0; i--) {
    if (/^\d+$/.test(words[i])) {
      numIdx = i;
      break;
    }
  }
  if (numIdx < 0 || volIdx >= numIdx) return null;
  const pct = Number(words[numIdx]);
  return pct >= 0 && pct <= 100 ? pct : null;
}

function extractMusicPlayTarget(words: string[], pills: readonly PillDefinition[]): "appleMusic" | "spotify" | null {
  for (const preposition of ["sur", "on"]) {
    const idx = words.lastIndexOf(preposition);
    if (idx < 0 || idx + 1 >= words.length) continue;
    const id = resolvePill(words.slice(idx + 1).join(" "), pills);
    if (id === "integration_spotify") return "spotify";
    if (id === "integration_music") return "appleMusic";
  }
  return null;
}

// ── Keyword tables (IntentParser.swift) ───────────────────────────────────────

const POLITENESS: string[][] = [
  ["je", "veux", "que", "tu"], ["je", "voudrais", "que", "tu"], ["j", "aimerais", "que", "tu"], ["faut", "que", "tu"],
  ["est", "ce", "que", "tu", "peux"], ["est", "ce", "que", "tu", "pourrais"], ["tu", "peux"], ["tu", "pourrais"],
  ["peux", "tu"], ["could", "you"], ["can", "you"], ["please"],
];

const INTERJECTIONS = new Set([
  "merci", "ouais", "oui", "ok", "okay", "bon", "alors", "euh", "bah", "ben", "hein",
  "voila", "donc", "hey", "yeah", "yes", "so", "well", "um", "uh", "thanks",
]);

const INFINITIVES: Record<string, string> = {
  mettre: "mets", lancer: "lance", jouer: "joue", ajouter: "ajoute", enlever: "enleve", retirer: "retire",
  supprimer: "supprime", activer: "active", desactiver: "desactive", remplacer: "remplace", garder: "garde",
  passer: "passe", monter: "monte", baisser: "baisse", couper: "coupe", arreter: "arrete",
  ajoutes: "ajoute", rajoutes: "rajoute", enleves: "enleve", retires: "retire", supprimes: "supprime",
  mettes: "mets", lances: "lance", actives: "active", desactives: "desactive", remplaces: "remplace",
  demarrer: "lance", demarre: "lance", demarres: "lance",
};

const PAUSE = [
  ["pause"], ["stop"], ["stoppe"], ["coupe"], ["stop", "la", "musique"], ["stop", "la", "chanson"],
  ["mets", "en", "pause"], ["met", "en", "pause"], ["arrete", "la", "musique"], ["arrete", "la", "chanson"],
  ["arrete", "la", "lecture"], ["arrete"], ["coupe", "la", "musique"],
];
const NEXT = [
  ["morceau", "suivant"], ["chanson", "suivante"], ["suivant"], ["prochain"], ["next", "track"], ["next", "song"],
  ["next"], ["skip"], ["chanson", "d", "apres"], ["d", "apres"], ["passe", "a", "la", "suivante"],
];
const PREVIOUS = [
  ["morceau", "precedent"], ["chanson", "precedente"], ["precedent"], ["en", "arriere"], ["previous", "track"],
  ["previous", "song"], ["previous"], ["back"], ["remets", "la", "chanson", "d", "avant"], ["chanson", "d", "avant"],
  ["reviens", "en", "arriere"],
];
const VOLUME_UP = [
  ["monte", "le", "son"], ["monte", "le", "volume"], ["augmente", "le", "son"], ["augmente", "le", "volume"],
  ["plus", "fort"], ["volume", "up"], ["louder"], ["turn", "up"],
];
const VOLUME_DOWN = [
  ["baisse", "le", "son"], ["baisse", "le", "volume"], ["diminue", "le", "son"], ["diminue", "le", "volume"],
  ["moins", "fort"], ["volume", "down"], ["quieter"], ["turn", "down"],
];
const PLAYLIST = [
  ["joue", "la", "playlist"], ["lance", "la", "playlist"], ["mets", "la", "playlist"], ["demarre", "la", "playlist"],
  ["play", "playlist"], ["start", "playlist"], ["balance", "la", "playlist"],
];
const PILL_MAIN = [
  ["passe", "sur"], ["change", "pour"], ["met", "sur"], ["mets", "sur"], ["switch", "to"], ["set", "main", "to"],
  ["change", "to"], ["utilise"], ["use"],
];
const MUSIC_OR_PILL = [
  ["joue"], ["play"], ["mets"], ["met"], ["lance"], ["start"], ["demarre"], ["balance"], ["envoie"], ["reprends"], ["resume"],
];
const PILL_ADD_ONLY = [
  ["active"], ["affiche"], ["montre"], ["rajoute"], ["ajoute"], ["add"], ["enable"], ["show"], ["activate"],
  ["je", "veux"], ["me", "faut"],
];
const PILL_REMOVE = [
  ["enleve"], ["supprime"], ["desactive"], ["cache"], ["retire"], ["efface"], ["remove"], ["disable"], ["hide"],
  ["delete"], ["vire"], ["degage"], ["enleve", "moi"], ["plus", "besoin", "de"],
];
const BARE_MUSIC_VERBS = new Set(["joue", "lance", "lecture", "play", "start", "demarre", "balance", "reprends", "resume"]);
const MUSIC_GENERIC = new Set(["musique", "son", "music", "audio", "chanson", "chansons"]);
const MUSIC_SERVICE_PILLS = new Set(["integration_music", "integration_spotify"]);
const PLAY = [
  ["lance", "la", "musique"], ["lance", "la", "chanson"], ["reprends", "la", "musique"], ["reprends"],
  ["play", "music"], ["start", "music"], ["resume", "music"], ["resume"], ["play", "some", "music"],
  ["balance", "de", "la", "musique"], ["balance", "la", "musique"], ["envoie", "de", "la", "musique"],
  ["mets", "de", "la", "musique"], ["mets", "du", "son"],
];

// ── Parser ────────────────────────────────────────────────────────────────────

export function parseIntent(raw: string, pills: readonly PillDefinition[]): Intent {
  const words = normalise(raw).split(" ").filter(Boolean);
  if (words.length === 0) return UNKNOWN;

  let [fwords, frawWords] = defilter(words, buildRawWords(raw));
  if (fwords.length === 0) return UNKNOWN;
  [fwords, frawWords] = stripTrailingLocation(fwords, frawWords);
  [fwords, frawWords] = stripTrailingInstead(fwords, frawWords);
  if (fwords.length === 0) return UNKNOWN;

  while (fwords.length > 1 && INTERJECTIONS.has(fwords[0])) {
    fwords = fwords.slice(1);
    frawWords = frawWords.slice(1);
  }
  for (const prefix of POLITENESS) {
    if (prefix.every((p, k) => fwords[k] === p)) {
      fwords = fwords.slice(prefix.length);
      frawWords = frawWords.slice(Math.min(prefix.length, frawWords.length));
      break;
    }
  }
  if (fwords.length === 0) return UNKNOWN;
  fwords = fwords.map((w) => INFINITIVES[w] ?? w);

  const insteadOf = extractInsteadOf(fwords, pills);
  if (insteadOf) return { kind: "pillReplace", old: insteadOf[0], new: insteadOf[1] };

  // "j'ai envie d'écouter du Drake"
  const listenAt = Math.max(fwords.lastIndexOf("ecouter"), fwords.lastIndexOf("ecoute"));
  if (listenAt >= 0 && (listenAt > 0 || fwords.length > 1)) {
    const articles = new Set(["du", "de", "des", "la", "le", "les", "l", "d", "un", "une"]);
    const restN = fwords.slice(listenAt + 1);
    const restR = frawWords.slice(Math.min(listenAt + 1, frawWords.length));
    while (restN.length && articles.has(restN[0])) {
      restN.shift();
      restR.shift();
    }
    if (restN.length) {
      if (MUSIC_GENERIC.has(restN.join(" "))) return { kind: "musicPlay", target: null };
      if (restN.join(" ") !== "ca" && restN.join(" ") !== "moi") {
        const r = restR.join(" ");
        return { kind: "musicPlaySearch", name: r || restN.join(" ") };
      }
    }
  }

  if (matchesAny(fwords, PAUSE)) return { kind: "musicPause" };
  if (matchesAny(fwords, NEXT)) return { kind: "musicNext" };
  if (matchesAny(fwords, PREVIOUS)) return { kind: "musicPrevious" };
  if (matchesAny(fwords, VOLUME_UP)) return { kind: "musicVolumeUp" };
  if (matchesAny(fwords, VOLUME_DOWN)) return { kind: "musicVolumeDown" };
  const volume = extractVolume(fwords);
  if (volume != null) return { kind: "musicSetVolume", percent: volume };
  const target = extractMusicPlayTarget(fwords, pills);
  if (target) return { kind: "musicPlay", target };

  // "principale/principal" → main pill
  const main = (id: string | null): Intent | null => (id ? { kind: "pillSetMain", id } : null);
  const workspace = (s: string) => {
    const clean = stripArticles(s);
    return clean ? resolvePill(clean, pills, "workspace") : null;
  };
  const saysMain = fwords.includes("principale") || fwords.includes("principal");
  if (saysMain) {
    const sur = fwords.indexOf("sur");
    if (sur >= 0 && sur + 1 < fwords.length) {
      const hit = main(workspace(fwords.slice(sur + 1).join(" ")));
      if (hit) return hit;
    }
    for (const sep of ["pour", "c est", "est"]) {
      const sepWords = sep.split(" ");
      const at = indexOfSequence(sepWords, fwords);
      if (at >= 0) {
        const hit = main(workspace(fwords.slice(at + sepWords.length).join(" ")));
        if (hit) return hit;
      }
    }
  }
  // "mets Cursor en principal[e]"
  for (let i = 1; i < fwords.length - 1; i++) {
    if (fwords[i] === "en" && (fwords[i + 1] === "principal" || fwords[i + 1] === "principale")) {
      const hit = main(workspace(fwords.slice(1, i).join(" ")));
      if (hit) return hit;
      break;
    }
  }
  // "Cursor comme pilule principale"
  if (fwords.includes("comme") && saysMain) {
    const at = fwords.indexOf("comme");
    if (at > 0) {
      const hit = main(workspace(fwords.slice(0, at).join(" ")));
      if (hit) return hit;
    }
  }

  const replace = extractReplace(fwords, pills);
  if (replace) return { kind: "pillReplace", old: replace[0], new: replace[1] };
  const only = extractOnly(fwords, pills);
  if (only) return { kind: "pillOnly", ids: only };

  const playlist = extractAfter(fwords, frawWords, PLAYLIST);
  if (playlist) {
    const clean = stripArticles(playlist.norm);
    const cleanRaw = stripArticlesRaw(playlist.raw);
    return { kind: "musicPlayPlaylist", name: clean ? cleanRaw || clean : "" };
  }
  const last = fwords[fwords.length - 1];
  if (last === "playlist" || last === "playlists") {
    const v = fwords.findIndex((w) => ["lance", "mets", "met", "joue", "balance", "play", "start"].includes(w));
    if (v >= 0) {
      const fillers = new Set(["my", "the", "a", "la", "le", "les", "ma", "mon", "une", "un"]);
      const name: string[] = [];
      for (let i = v + 1; i < fwords.length - 1; i++) {
        if (!fillers.has(fwords[i])) name.push(i < frawWords.length ? frawWords[i] : fwords[i]);
      }
      return { kind: "musicPlayPlaylist", name: name.join(" ") };
    }
  }

  // "make Cursor my main pill"
  if (fwords.includes("main") && (fwords.includes("pill") || fwords.includes("pilule"))) {
    const noise = new Set(["make", "set", "as", "my", "the", "main", "pill", "to", "switch", "change", "use", "put", "is", "be", "should", "please", "now"]);
    const rest = fwords.filter((w) => !noise.has(w)).join(" ");
    const id = rest ? resolvePill(rest, pills, "workspace") : null;
    if (id) return { kind: "pillSetMain", id };
  }

  const mainMatch = extractAfter(fwords, frawWords, PILL_MAIN);
  if (mainMatch) {
    const clean = stripArticles(mainMatch.norm);
    if (clean) {
      const id = resolvePill(clean, pills, "workspace");
      return id ? { kind: "pillSetMain", id } : UNKNOWN;
    }
  }

  const ambiguous = extractAfter(fwords, frawWords, MUSIC_OR_PILL);
  if (ambiguous) {
    const hasPilule = fwords.includes("pilule") || fwords.includes("pill");
    const clean = stripArticles(ambiguous.norm);
    const cleanRaw = stripArticlesRaw(ambiguous.raw);
    if (clean) {
      if (splitByConjunction(clean).length >= 2) {
        const ids = resolveAll(clean, pills);
        if (ids.length >= 2) return { kind: "pillAddMultiple", ids };
      }
      if (MUSIC_GENERIC.has(clean)) return { kind: "musicPlay", target: null };
      const any = resolvePill(clean, pills);
      if (any && MUSIC_SERVICE_PILLS.has(any)) {
        return { kind: "musicPlay", target: any === "integration_spotify" ? "spotify" : "appleMusic" };
      }
      if (hasPilule && !saysMain) return any ? { kind: "pillAdd", id: any } : UNKNOWN;
      const ws = resolvePill(clean, pills, "workspace");
      if (ws) return { kind: "pillSetMain", id: ws };
      if (any) return { kind: "pillAdd", id: any };
      return { kind: "musicPlaySearch", name: cleanRaw || clean };
    }
  }

  const removeMatch = extractAfter(fwords, frawWords, PILL_REMOVE);
  if (removeMatch) {
    const clean = stripArticles(removeMatch.norm);
    if (clean) {
      if (splitByConjunction(clean).length >= 2) {
        const ids = resolveAll(clean, pills);
        if (ids.length >= 2) return { kind: "pillRemoveMultiple", ids };
      }
      const id = resolvePill(clean, pills);
      return id ? { kind: "pillRemove", id } : UNKNOWN;
    }
  }

  const addMatch = extractAfter(fwords, frawWords, PILL_ADD_ONLY);
  if (addMatch) {
    const clean = stripArticles(addMatch.norm);
    if (clean) {
      if (splitByConjunction(clean).length >= 2) {
        const ids = resolveAll(clean, pills);
        if (ids.length >= 2) return { kind: "pillAddMultiple", ids };
      }
      const id = resolvePill(clean, pills);
      return id ? { kind: "pillAdd", id } : UNKNOWN;
    }
  }

  if (matchesAny(fwords, PLAY)) return { kind: "musicPlay", target: null };
  if (fwords.length === 1 && BARE_MUSIC_VERBS.has(fwords[0])) return { kind: "musicPlay", target: null };
  return UNKNOWN;
}

// ── Several actions in one sentence (parseMultiAction) ───────────────────────

const SEGMENT_VERBS = new Set([
  "ajoute", "ajouter", "rajoute", "rajouter", "mets", "met", "mettre", "remets", "remettre",
  "active", "activer", "enleve", "enlever", "retire", "retirer", "supprime", "supprimer",
  "vire", "virer", "degage", "degager", "desactive", "desactiver", "cache", "cacher",
  "lance", "lancer", "joue", "jouer", "add", "remove", "put", "enable", "disable", "play",
]);
const JOINS = new Set(["et", "puis", "ensuite", "and", "then"]);

export function splitOnActionVerbs(segment: string): string[] {
  const parts: string[][] = [];
  let cur: string[] = [];
  let curHasVerb = false;
  for (const w of segment.split(" ").filter(Boolean)) {
    const isVerb = SEGMENT_VERBS.has(normalise(w));
    if (isVerb && curHasVerb) {
      parts.push(cur);
      cur = [];
      curHasVerb = false;
    }
    cur.push(w);
    if (isVerb) curHasVerb = true;
  }
  if (cur.length) parts.push(cur);
  return parts
    .map((p) => {
      const q = [...p];
      while (q.length && JOINS.has(normalise(q[q.length - 1]))) q.pop();
      while (q.length && JOINS.has(normalise(q[0]))) q.shift();
      return q.join(" ");
    })
    .filter(Boolean);
}

/** Two or more intents in one sentence, or null (then `parseIntent` decides alone). */
export function parseMultiAction(raw: string, pills: readonly PillDefinition[]): Intent[] | null {
  const pieces = raw
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean)
    .flatMap(splitOnActionVerbs);
  const intents: Intent[] = [];
  for (const piece of pieces) {
    const parts: string[][] = [];
    let cur: string[] = [];
    for (const w of normalise(piece).split(" ").filter(Boolean)) {
      if (JOINS.has(w)) {
        if (cur.length) parts.push(cur);
        cur = [];
      } else cur.push(w);
    }
    if (cur.length) parts.push(cur);
    const sub = parts.map((p) => parseIntent(p.join(" "), pills));
    if (parts.length >= 2 && sub.every((s) => s.kind !== "unknown")) {
      intents.push(...sub);
      continue;
    }
    const whole = parseIntent(piece, pills);
    if (whole.kind === "unknown") return null;
    intents.push(whole);
  }
  return intents.length >= 2 ? intents : null;
}

const OPEN_VERBS = new Set(["ouvre", "ouvrir", "open"]);
const OPEN_FILLERS = new Set(["moi", "l", "la", "le", "app", "application", "the", "appli"]);
/** Longer than this after "open", it is a sentence being dictated, not an app's name. */
const MAX_APP_WORDS = 4;

/**
 * "ouvre Figma", "open the app Notion" → the app's name as it was said
 * (VoiceQuery.appToOpen): the words after the first open verb, past "moi",
 * articles and "app". Null when the sentence opens nothing. Unlike the Mac,
 * more than four words after the verb is dictation ("…open the chat and show
 * me the latest pull request"), not a name: Whisper hears whole sentences.
 */
export function appToOpen(raw: string): string | null {
  const n = normalise(raw).split(" ").filter(Boolean);
  const r = buildRawWords(raw);
  if (n.length !== r.length) return null;
  const i = n.findIndex((w) => OPEN_VERBS.has(w));
  if (i < 0 || i + 1 >= n.length) return null;
  let start = i + 1;
  while (start < n.length && OPEN_FILLERS.has(n[start])) start++;
  if (start >= n.length || n.length - start > MAX_APP_WORDS) return null;
  const name = r.slice(start).join(" ").replace(/^[\p{P}\s]+|[\p{P}\s]+$/gu, "");
  return name || null;
}

/** Everything a transcript asks for: one intent, several, or nothing. */
export function parseTranscript(raw: string, pills: readonly PillDefinition[]): Intent[] {
  // Whisper ends sentences with punctuation; the Mac's recogniser didn't.
  const text = raw.trim().replace(/[.!?…]+$/u, "").trim();
  // Before the pills, as on the Mac: "ouvre Figma" opens Figma.
  const app = appToOpen(text);
  if (app) return [{ kind: "openApp", name: app }];
  const multi = parseMultiAction(text, pills);
  if (multi) return multi;
  const one = parseIntent(text, pills);
  return one.kind === "unknown" ? [] : [one];
}
