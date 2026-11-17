// UI state: the latest AppState snapshot plus a few view-only bits. All
// backend traffic goes through api.ts.

import {
  call,
  emptyState,
  on,
  toApiError,
  type ApiError,
  type AppState,
  type CommandName,
  type Commands,
} from './api';

/** Sections of the right-hand panel (EQ, Effects and Mic are always on screen). */
export type Tab = 'mix' | 'out' | 'prof' | 'sys';

const TOAST_MS = 5000;

class AppStore {
  state = $state<AppState>(emptyState());
  /** False until the first snapshot arrived. */
  loaded = $state(false);
  tab = $state<Tab>('mix');
  toast = $state<{ id: number; message: string } | null>(null);

  private toastId = 0;
  private toastTimer: ReturnType<typeof setTimeout> | null = null;
  private started = false;

  /** Listen first, then read, then keep the snapshot with the higher revision. */
  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;
    await on('state-changed', (s) => this.apply(s));
    await on('device-error', (e) => this.notify(e));
    // Presets can change outside the app (`g6 preset`): re-list when the panel opens. The backend
    // announces a changed list with a `state-changed`.
    await on('panel-shown', () => void this.run('list_presets'));
    try {
      this.apply(await call('get_state'));
    } catch (e) {
      this.notify(toApiError(e));
    }
  }

  /** Apply a snapshot unless a newer one is already shown. */
  apply(s: AppState): void {
    if (this.loaded && s.revision < this.state.revision) return;
    this.state = s;
    this.loaded = true;
  }

  /** Run a command, apply the snapshot it returns, show a failure as a notice. */
  async run<K extends CommandName>(
    cmd: K,
    args?: Commands[K]['args'],
  ): Promise<Commands[K]['result'] | undefined> {
    try {
      const r = await call(cmd, args);
      if (r && typeof r === 'object' && 'revision' in r) this.apply(r as AppState);
      return r;
    } catch (e) {
      this.notify(toApiError(e));
      return undefined;
    }
  }

  notify(e: ApiError): void {
    this.say(e.message || e.code);
  }

  /** Show a transient notice (also used for plain confirmations). */
  say(message: string): void {
    const id = ++this.toastId;
    this.toast = { id, message };
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => {
      if (this.toast?.id === id) this.toast = null;
    }, TOAST_MS);
  }

  dismissToast(): void {
    this.toast = null;
  }
}

export const app = new AppStore();
