// Read aloud (release plan feature 8): one reply plays at a time. The core
// turns the reply's Markdown into short spoken pieces (prose only) and
// speaks each with Windows' voices in a sandboxed worker; this plays them in
// order, asking for the next piece while the current one plays.
import { useSyncExternalStore } from "react";
import { api, errorText } from "./api";

type Status = { key: string | null; loading: boolean; error: string | null; errorKey: string | null };

let status: Status = { key: null, loading: false, error: null, errorKey: null };

/** A reply's key: its text, so a finished reply read automatically and its
 * saved message share one Stop button. */
export function speechKey(text: string): string {
  return `${text.length}:${text.slice(0, 80)}`;
}
const listeners = new Set<() => void>();
let run = 0;
let audio: HTMLAudioElement | null = null;
let autoRead = false;

function set(next: Partial<Status>) {
  status = { ...status, ...next };
  for (const l of listeners) l();
}

function wavUrl(base64: string): string {
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return URL.createObjectURL(new Blob([bytes], { type: "audio/wav" }));
}

function play(url: string, token: number): Promise<void> {
  return new Promise((resolve, reject) => {
    if (token !== run) return resolve();
    const a = new Audio(url);
    audio = a;
    const done = () => {
      URL.revokeObjectURL(url);
      resolve();
    };
    a.onended = done;
    a.onpause = () => {
      if (token !== run) done();
    };
    a.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("the audio couldn't be played"));
    };
    a.play().catch(reject);
  });
}

/** Stop whatever is being read. */
export function stopReading() {
  run++;
  audio?.pause();
  audio = null;
  set({ key: null, loading: false });
}

/** Read `text` (a reply's Markdown) aloud; its button shows Stop. Reading
 * another reply stops this one. */
export async function readAloud(text: string) {
  stopReading();
  const key = speechKey(text);
  const token = ++run;
  set({ key, loading: true, error: null, errorKey: null });
  try {
    const pieces = await api.speechPrepare(text);
    if (!pieces.length) throw new Error("there's no text to read in this reply");
    let next: Promise<string> | null = api.speakPiece(pieces[0]!);
    for (let i = 0; i < pieces.length; i++) {
      const wav = await next!;
      if (token !== run) return;
      next = i + 1 < pieces.length ? api.speakPiece(pieces[i + 1]!) : null;
      if (i === 0) set({ loading: false });
      await play(wavUrl(wav), token);
      if (token !== run) return;
    }
    if (token === run) set({ key: null, loading: false });
  } catch (e) {
    if (token === run) set({ key: null, loading: false, error: errorText(e), errorKey: key });
  }
}

let voiceInputOn = true;
const voiceListeners = new Set<() => void>();

/** Settings → Voice, kept in step by App: "read new replies automatically"
 * and whether voice input is on (the microphone is hidden when it's off). */
export function syncVoice(readAuto: boolean, inputOn: boolean) {
  autoRead = readAuto;
  if (inputOn !== voiceInputOn) {
    voiceInputOn = inputOn;
    for (const l of voiceListeners) l();
  }
}

export function useVoiceInputOn(): boolean {
  return useSyncExternalStore(
    (l) => {
      voiceListeners.add(l);
      return () => voiceListeners.delete(l);
    },
    () => voiceInputOn,
  );
}

/** Read a reply that just finished, when the user turned that on. */
export function readIfAuto(text: string) {
  if (autoRead && text.trim()) void readAloud(text);
}

export function useSpeech(): Status {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => status,
  );
}

export function clearSpeechError() {
  set({ error: null, errorKey: null });
}
