// Dark (pitch black, the default) or light (pure white) theme. The choice is a
// per-machine UI preference, kept in localStorage; the window's title bar and
// background follow it.
import { inTauri } from './api';

export type Theme = 'dark' | 'light';

const KEY = 'opendac.theme';

function load(): Theme {
  try {
    return localStorage.getItem(KEY) === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

export const theme = $state<{ value: Theme }>({ value: load() });

/** Apply `t` to the page and, inside Tauri, to the window chrome. */
export function applyTheme(t: Theme): void {
  document.documentElement.dataset.theme = t;
  if (!inTauri) return;
  void import('@tauri-apps/api/window').then(({ getCurrentWindow }) => {
    const w = getCurrentWindow();
    w.setTheme(t).catch(() => {});
    w.setBackgroundColor(t === 'light' ? '#ffffff' : '#000000').catch(() => {});
  });
}

export function toggleTheme(): void {
  theme.value = theme.value === 'dark' ? 'light' : 'dark';
  try {
    localStorage.setItem(KEY, theme.value);
  } catch {
    // Not stored (e.g. storage blocked): still applies for this session.
  }
  applyTheme(theme.value);
}
