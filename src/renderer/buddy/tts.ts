// Web Speech API wrapper for buddy speech.
// Uses the browser's built-in speechSynthesis — no API keys, no network,
// uses OS voices on macOS (Samantha, Daniel, Siri voices when installed).

const MAX_SPEAK_CHARS = 180;

export interface SpeakOptions {
  voice?: string | null;
  rate?: number;
  pitch?: number;
}

function trimForSpeech(text: string): string {
  const cleaned = text
    .replace(/\*+/g, '')
    .replace(/[`]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
  if (cleaned.length <= MAX_SPEAK_CHARS) return cleaned;
  const slice = cleaned.slice(0, MAX_SPEAK_CHARS);
  const lastBreak = Math.max(slice.lastIndexOf('. '), slice.lastIndexOf('! '), slice.lastIndexOf('? '));
  return (lastBreak > 80 ? slice.slice(0, lastBreak + 1) : slice).trim();
}

export function isTtsAvailable(): boolean {
  return typeof window !== 'undefined' && 'speechSynthesis' in window;
}

export function speak(text: string, opts: SpeakOptions = {}) {
  if (!isTtsAvailable()) return;
  const trimmed = trimForSpeech(text);
  if (!trimmed) return;

  // Cancel anything in flight so reactions don't pile up.
  window.speechSynthesis.cancel();

  const utter = new SpeechSynthesisUtterance(trimmed);
  if (opts.rate != null) utter.rate = clamp(opts.rate, 0.5, 2);
  if (opts.pitch != null) utter.pitch = clamp(opts.pitch, 0, 2);

  const voices = window.speechSynthesis.getVoices();
  if (opts.voice) {
    const match = voices.find((v) => v.name === opts.voice);
    if (match) utter.voice = match;
  } else {
    const englishDefault =
      voices.find((v) => v.default && v.lang.toLowerCase().startsWith('en')) ??
      voices.find((v) => v.lang.toLowerCase().startsWith('en'));
    if (englishDefault) utter.voice = englishDefault;
  }
  utter.lang = utter.voice?.lang ?? 'en-US';

  window.speechSynthesis.speak(utter);
}

export function stopSpeaking() {
  if (!isTtsAvailable()) return;
  window.speechSynthesis.cancel();
}

export function getVoicesAsync(): Promise<SpeechSynthesisVoice[]> {
  if (!isTtsAvailable()) return Promise.resolve([]);
  const immediate = window.speechSynthesis.getVoices();
  if (immediate.length) return Promise.resolve(immediate);
  return new Promise((resolve) => {
    const handler = () => {
      window.speechSynthesis.removeEventListener('voiceschanged', handler);
      resolve(window.speechSynthesis.getVoices());
    };
    window.speechSynthesis.addEventListener('voiceschanged', handler);
    setTimeout(() => {
      window.speechSynthesis.removeEventListener('voiceschanged', handler);
      resolve(window.speechSynthesis.getVoices());
    }, 1500);
  });
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}
