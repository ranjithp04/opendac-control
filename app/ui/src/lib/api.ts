// The UI <-> Rust contract: types, plus the only place the UI talks to Rust
// (`call`, `on`). The contract is app/API.md; the Rust side is
// app/src-tauri/src/api.rs and app/src-tauri/src/commands.rs. Change all of
// them together.
//
// Inside Tauri, `call` is `invoke` and `on` is `listen`. In a plain browser
// (`npm run dev`) the in-memory mock backend (mock.ts) answers instead.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

// --- Enums ---------------------------------------------------------------

/** Active output. Label the edited profile "Headphones" / "Speakers". */
export type Output = 'headphones' | 'speakers';

export type Connection = 'connected' | 'disconnected';

export type DacFilter = 'fast-min' | 'slow-min' | 'nos' | 'fast-lin' | 'slow-lin';

/** Dolby Digital decoder mode (optical input). */
export type DolbyMode = 'full' | 'normal' | 'night';

export type SmartVolumeMode = 'normal' | 'loud' | 'night';

/** Mic boost in dB. */
export type MicBoost = 0 | 10 | 20 | 30;

/** SBX effects that have an on/off and a level. */
export type EffectName = 'surround' | 'crystalizer' | 'bass' | 'dialog' | 'smartVolume';

// --- Snapshot ------------------------------------------------------------

export interface Eq {
  enabled: boolean;
  /** dB for 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz; -12..12, step 0.5. Length 10. */
  bands: number[];
  /** Pre-amp in dB; -6..6, step 0.5. Per output. Not owned by Scout, untouched by Direct Mode. */
  preamp: number;
}

/** A user-saved EQ curve: the 10 bands and the pre-amp (dB). */
export interface EqCurve {
  name: string;
  /** Length 10. */
  bands: number[];
  preamp: number;
}

export interface Effect {
  enabled: boolean;
  /** 0..1 */
  level: number;
}

export interface Surround extends Effect {
  /** 10..300, step 0.5 */
  distance: number;
}

export interface SmartVolume extends Effect {
  mode: SmartVolumeMode;
}

export interface Effects {
  surround: Surround;
  crystalizer: Effect;
  bass: Effect;
  dialog: Effect;
  smartVolume: SmartVolume;
}

/** All fields except `boost` are the active output's values (stored per output). */
export interface Mic {
  /** null until read. Shared by both outputs. */
  boost: MicBoost | null;
  voiceClarity: boolean;
  /** Voice Clarity noise reduction, 0..1. */
  noiseReduction: number;
  /** Mic EQ on/off. The 8 band gains are not exposed. */
  eqEnabled: boolean;
  /** Acoustic echo cancellation. */
  aec: boolean;
  /** Recording Smart Volume. */
  smartVolume: boolean;
}

/**
 * Full snapshot of everything the panels show. Returned by `get_state` and by
 * every write command, and carried by the `state-changed` event.
 */
export interface AppState {
  /** Increases with every snapshot; drop snapshots older than the one shown. */
  revision: number;
  connection: Connection;
  /** e.g. "2.1.250903.1324"; null until read. */
  firmware: string | null;
  /** null until first read. */
  output: Output | null;
  /** Our Direct Mode. While on, EQ / SBX / Scout are off and read-only. */
  directMode: boolean;
  sbx: boolean;
  scout: boolean;
  eq: Eq;
  effects: Effects;
  mic: Mic;
  dacFilter: DacFilter | null;
  dolbyDecoder: DolbyMode | null;
  /** Preset names, sorted. */
  presets: string[];
  /** Last preset applied or saved ("Preset: X" footer). */
  activePreset: string | null;
  /** User-saved EQ curves in saved order. The built-in curves live in eqCurves.ts. */
  eqCurves: EqCurve[];
  launchAtLogin: boolean;
}

// --- Errors --------------------------------------------------------------

export type ErrorCode =
  | 'notConnected'
  | 'directModeActive'
  | 'scoutActive'
  | 'invalidValue'
  | 'deviceTimeout'
  | 'deviceRejected'
  | 'deviceMismatch'
  | 'incomplete'
  | 'presetNotFound'
  | 'presetInvalidName'
  | 'presetNoProfile'
  | 'curveNotFound'
  | 'storage'
  | 'autostart'
  | 'internal'
  | 'notImplemented';

/** What a rejected command promise carries, and the `device-error` payload. */
export interface ApiError {
  code: ErrorCode;
  message: string;
}

// --- Events (Rust -> UI) -------------------------------------------------

export interface ConnectionChanged {
  connection: Connection;
  firmware: string | null;
}

export interface OutputChanged {
  output: Output;
}

/** Event name -> payload. */
export interface Events {
  /** Authoritative. Emitted after every change, coalesced to at most one per ~30 ms. */
  'state-changed': AppState;
  /** Hint, emitted just before `state-changed`. */
  'connection-changed': ConnectionChanged;
  /** Hint, emitted just before `state-changed`. */
  'output-changed': OutputChanged;
  /** A failure the UI did not ask for: Direct Mode could not be extended to (or restored on) the output that became active. Failed re-apply steps on reconnect are only logged. */
  'device-error': ApiError;
  /** The panel was just shown. Payload is null. */
  'panel-shown': null;
}

export type EventName = keyof Events;

// --- Commands (UI -> Rust) -----------------------------------------------

type NoArgs = Record<string, never>;

/**
 * Command name -> argument object (camelCase keys, as passed to `invoke`) and
 * resolved value. A rejected promise carries an `ApiError`, except when
 * argument deserialisation fails (e.g. an unknown enum string), which rejects
 * with a plain string.
 */
export interface Commands {
  // State
  get_state: { args: NoArgs; result: AppState };
  refresh_state: { args: NoArgs; result: AppState };

  // EQ, SBX, Scout
  set_eq_enabled: { args: { enabled: boolean }; result: AppState };
  /** band 0..9, gain -12..12 dB in 0.5 steps. */
  set_eq_band: { args: { band: number; gain: number }; result: AppState };
  /** gains: exactly 10 values. */
  set_eq_bands: { args: { gains: number[] }; result: AppState };
  /** db -6..6 in 0.5 steps. Allowed while Scout is on. */
  set_eq_preamp: { args: { db: number }; result: AppState };
  /** Writes all 10 bands and the pre-amp, and turns EQ on. */
  apply_eq_curve: { args: { bands: number[]; preamp: number }; result: AppState };

  // User EQ curves
  list_eq_curves: { args: NoArgs; result: EqCurve[] };
  /** Saves the active output's current bands and pre-amp (replaces a curve with the same name). */
  save_eq_curve: { args: { name: string }; result: AppState };
  delete_eq_curve: { args: { name: string }; result: AppState };
  set_sbx: { args: { enabled: boolean }; result: AppState };
  set_scout: { args: { enabled: boolean }; result: AppState };

  // Effects
  set_effect_enabled: { args: { effect: EffectName; enabled: boolean }; result: AppState };
  /** level 0..1 */
  set_effect_level: { args: { effect: EffectName; level: number }; result: AppState };
  /** distance 10..300 in 0.5 steps. */
  set_surround_distance: { args: { distance: number }; result: AppState };
  set_smart_volume_mode: { args: { mode: SmartVolumeMode }; result: AppState };

  // Mic
  set_mic_boost: { args: { db: MicBoost }; result: AppState };
  set_voice_clarity: { args: { enabled: boolean }; result: AppState };
  /** level 0..1 */
  set_noise_reduction: { args: { level: number }; result: AppState };
  set_mic_eq_enabled: { args: { enabled: boolean }; result: AppState };
  set_mic_aec: { args: { enabled: boolean }; result: AppState };
  set_mic_smart_volume: { args: { enabled: boolean }; result: AppState };

  // Device
  set_dac_filter: { args: { filter: DacFilter }; result: AppState };
  set_dolby_decoder: { args: { mode: DolbyMode }; result: AppState };
  set_direct_mode: { args: { enabled: boolean }; result: AppState };

  // Presets
  list_presets: { args: NoArgs; result: string[] };
  save_preset: { args: { name: string }; result: AppState };
  apply_preset: { args: { name: string }; result: AppState };
  rename_preset: { args: { from: string; to: string }; result: AppState };
  delete_preset: { args: { name: string }; result: AppState };

  // App
  set_launch_at_login: { args: { enabled: boolean }; result: AppState };
  hide_panel: { args: NoArgs; result: null };
  set_app_theme: { args: { theme: 'light' | 'dark' }; result: null };
}

export type CommandName = keyof Commands;

// --- Transport -----------------------------------------------------------

/** True inside the Tauri webview, false in a plain browser. */
export const inTauri: boolean =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** The mock backend has the same two entry points as the real transport. */
export interface Backend {
  invoke(cmd: string, args: Record<string, unknown>): Promise<unknown>;
  listen(name: string, handler: (payload: unknown) => void): () => void;
}

let mock: Promise<Backend> | null = null;
function mockBackend(): Promise<Backend> {
  // Loaded on demand: never fetched inside Tauri.
  mock ??= import('./mock').then((m) => m.createMockBackend());
  return mock;
}

/** Normalise whatever a rejected `invoke` carries into an `ApiError`. */
export function toApiError(e: unknown): ApiError {
  if (e && typeof e === 'object' && 'code' in e && 'message' in e) {
    const { code, message } = e as { code: unknown; message: unknown };
    return { code: code as ErrorCode, message: String(message) };
  }
  // Argument deserialisation failures reject with a plain string (a UI bug).
  return { code: 'internal', message: typeof e === 'string' ? e : String(e) };
}

/** Call a command. Rejects with an `ApiError`. */
export async function call<K extends CommandName>(
  cmd: K,
  args?: Commands[K]['args'],
): Promise<Commands[K]['result']> {
  try {
    const a = (args ?? {}) as Record<string, unknown>;
    const r = inTauri ? await invoke(cmd, a) : await (await mockBackend()).invoke(cmd, a);
    return r as Commands[K]['result'];
  } catch (e) {
    throw toApiError(e);
  }
}

/** Subscribe to an event. Resolves with the unsubscribe function. */
export async function on<K extends EventName>(
  name: K,
  handler: (payload: Events[K]) => void,
): Promise<() => void> {
  if (inTauri) {
    return listen<Events[K]>(name, (e) => handler(e.payload));
  }
  const b = await mockBackend();
  return b.listen(name, (p) => handler(p as Events[K]));
}

// --- Neutral snapshot ----------------------------------------------------

/** What the UI shows before the first snapshot arrives. */
export function emptyState(): AppState {
  const fx = { enabled: false, level: 0 };
  return {
    revision: 0,
    connection: 'disconnected',
    firmware: null,
    output: null,
    directMode: false,
    sbx: false,
    scout: false,
    eq: { enabled: false, bands: new Array<number>(10).fill(0), preamp: 0 },
    effects: {
      surround: { ...fx, distance: 10 },
      crystalizer: { ...fx },
      bass: { ...fx },
      dialog: { ...fx },
      smartVolume: { ...fx, mode: 'normal' },
    },
    mic: {
      boost: null,
      voiceClarity: false,
      noiseReduction: 0,
      eqEnabled: false,
      aec: false,
      smartVolume: false,
    },
    dacFilter: null,
    dolbyDecoder: null,
    presets: [],
    activePreset: null,
    eqCurves: [],
    launchAtLogin: false,
  };
}
