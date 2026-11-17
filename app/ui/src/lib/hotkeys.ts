// Global hotkey helpers: turn a key press into an accelerator string (what
// the Device tab sends), and a reduced port of the backend's validation for
// the browser mock. The backend (`hotkeys.rs`) is the authority: it validates
// and spells every accelerator, the UI shows what it returns.

const IS_MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);
/** The Command / Windows key, as the backend spells it. */
const META = IS_MAC ? 'Cmd' : 'Super';

/** The Command / Windows key as the user knows it (for hints). */
export const META_LABEL = IS_MAC ? 'Cmd' : 'Win';

/** Keys the backend accepts besides letters, digits and F1..F24. `KeyboardEvent.code` uses the same names. */
const NAMED_KEYS = new Set([
  'Space', 'Tab', 'Enter', 'Backspace', 'Delete', 'Insert', 'Home', 'End', 'PageUp', 'PageDown',
  'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Minus', 'Equal', 'Comma', 'Period',
  'Slash', 'Semicolon', 'Quote', 'Backquote', 'Backslash', 'BracketLeft', 'BracketRight',
]);

/** Key name for a `KeyboardEvent.code`, or null for keys that cannot be a hotkey (modifiers, Escape, ...). */
function keyName(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1];
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return NAMED_KEYS.has(code) ? code : null;
}

/** The accelerator for a key press, or null while only modifiers are down / the key is not usable. */
export function acceleratorFromEvent(e: KeyboardEvent): string | null {
  const key = keyName(e.code);
  if (key === null) return null;
  const parts: string[] = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push(META);
  parts.push(key);
  return parts.join('+');
}

/** Suggested accelerators (as the backend's `HotkeyAction::default_accelerator`). */
export function defaultAccelerator(key: 'D' | 'P' | 'S'): string {
  return IS_MAC ? `Ctrl+Alt+${META}+${key}` : `Ctrl+Alt+Shift+${key}`;
}

/** Reduced port of the backend's `normalize`, for the mock. Throws an Error with the reason. */
export function normalizeAccelerator(text: string): string {
  let ctrl = false;
  let alt = false;
  let shift = false;
  let meta = false;
  let key: string | null = null;
  for (const raw of text.split('+')) {
    const t = raw.trim();
    if (!t) throw new Error(`"${text.trim()}" has an empty part.`);
    switch (t.toLowerCase()) {
      case 'ctrl': case 'control': ctrl = true; break;
      case 'alt': case 'option': case 'opt': alt = true; break;
      case 'shift': shift = true; break;
      case 'cmd': case 'command': case 'super': case 'meta': case 'win': case 'windows': meta = true; break;
      default: {
        let k: string | null = null;
        if (/^[a-z0-9]$/i.test(t)) k = t.toUpperCase();
        else if (/^f([1-9]|1[0-9]|2[0-4])$/i.test(t)) k = t.toUpperCase();
        else k = [...NAMED_KEYS].find((n) => n.toLowerCase() === t.toLowerCase()) ?? null;
        if (k === null) throw new Error(`"${t}" is not a key that can be used.`);
        if (key !== null) throw new Error(`"${text.trim()}" has more than one key.`);
        key = k;
      }
    }
  }
  if (key === null) throw new Error(`"${text.trim()}" has no key.`);
  if (!(ctrl || alt || meta)) throw new Error(`"${text.trim()}" needs Ctrl, Alt or ${META} as well.`);
  return [ctrl && 'Ctrl', alt && 'Alt', shift && 'Shift', meta && META, key].filter(Boolean).join('+');
}
