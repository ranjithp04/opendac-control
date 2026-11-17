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
  /** Pre-amp in dB; -6..6, step 0.5. Per output, untouched by Direct Mode. The G6 sets it to 0 while Scout is on and restores it after; not locked by Scout. */
  preamp: number;
}

/** A user-saved EQ curve: the 10 bands and the pre-amp (dB). */
export interface EqCurve {
  name: string;
  /** Length 10. */
  bands: number[];
  preamp: number;
}

/** A user-saved mic EQ curve: the 8 band gains (dB). */
export interface MicEqCurve {
  name: string;
  /** Length 8. */
  bands: number[];
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
  /** Mic EQ on/off. */
  eqEnabled: boolean;
  /** dB for mic EQ bands 1..8; -12..12, step 0.5. Length 8. The band frequencies are unknown. */
  eqBands: number[];
  /** Acoustic echo cancellation. */
  aec: boolean;
  /** Recording Smart Volume. */
  smartVolume: boolean;
}

/** Which USB audio controls a mixer command addresses. */
export type MixerScope = 'playback' | 'monitoring' | 'recording';

/** Volume and mute of one scope. Both channels are always written together. */
export interface MixerLevel {
  /** 0..1, CoreAudio's volume scalar (mean of the channels). */
  volume: number;
  /** What the driver says `volume` is in dB; null if it did not tell. */
  volumeDb: number | null;
  /** null: the device offers no mute for this scope. */
  mute: boolean | null;
}

export interface MixerSource {
  id: number;
  name: string;
}

/**
 * The G6's USB Audio Class controls as the OS exposes them (not HID). A scope
 * the OS does not offer is null. Only macOS (CoreAudio) is implemented.
 */
export interface Mixer {
  /** Speaker volume / mute. */
  playback: MixerLevel | null;
  /** Monitoring: what is passed from the inputs to the output. */
  monitoring: MixerLevel | null;
  /** Recording level / mute of the selected source. */
  recording: MixerLevel | null;
  recordingSource: { current: number; options: MixerSource[] } | null;
}

/** LED lighting. `color` is lower-case `rrggbb` without `#`; "ffffff" while unknown. */
export interface Lighting {
  enabled: boolean;
  color: string;
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
  lighting: Lighting;
  /** Preset names, sorted. */
  presets: string[];
  /** Last preset applied or saved ("Preset: X" footer). */
  activePreset: string | null;
  /** User-saved EQ curves in saved order. The built-in curves live in eqCurves.ts. */
  eqCurves: EqCurve[];
  /** User-saved mic EQ curves in saved order. The built-in presets live in micEqCurves.ts. */
  micEqCurves: MicEqCurve[];
  /** Profile to apply when the output becomes headphones / speakers (null: none). */
  autoSwitch: AutoSwitch;
  launchAtLogin: boolean;
  /** Global hotkeys (opt-in) and whether each could be registered. */
  hotkeys: Hotkeys;
  /** null where the OS offers no mixer (not macOS) or the G6 audio device is not found: hide the Mixer tab. */
  mixer: Mixer | null;
}

/** One global hotkey. */
export interface HotkeyBinding {
  /** Canonical accelerator as the backend spells it, e.g. "Ctrl+Alt+Cmd+D". */
  accelerator: string;
  /** Why it could not be registered (e.g. another app uses it); null when it is registered or the hotkeys are off. */
  error: string | null;
}

export interface Hotkeys {
  enabled: boolean;
  toggleDirectMode: HotkeyBinding;
  nextProfile: HotkeyBinding;
  toggleSbx: HotkeyBinding;
}

/** Auto-switch rules, by output. A rule naming a deleted profile is cleared by the backend. */
export type AutoSwitch = Record<Output, string | null>;

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
  | 'mixerUnavailable'
  | 'internal'
  | 'notImplemented';

/** Result of `import_eq`: an AutoEq / Equalizer APO text mapped onto the bands and pre-amp. Nothing is written. */
export interface EqImport {
  /** Length 10; multiples of 0.5 within +-12 dB. */
  bands: number[];
  /** Multiple of 0.5 within +-6 dB. */
  preamp: number;
  /** What was clamped, approximated or ignored. */
  notes: string[];
}

/** Size limits the Rust side enforces on imported text (`g6_core::preset::MAX_FILE_BYTES`, `g6_core::eq_import::MAX_INPUT_BYTES`). Checked before reading a chosen file. */
export const MAX_PRESET_FILE_BYTES = 256 * 1024;
export const MAX_EQ_IMPORT_BYTES = 1024 * 1024;

/** What a rejected command promise carries, and the `device-error` payload. */
export interface ApiError {
  code: ErrorCode;
  message: string;
}

// --- Events (Rust -> UI) -------------------------------------------------

/** Result of `get_device_info`. A field the device did not answer is null. Hex strings are lowercase. */
export interface DeviceInfo {
  firmware: string | null;
  serial: string | null;
  dspVersion: string | null;
  hardwareId: string | null;
}

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
  /** Cached per connection; never rejects. */
  get_device_info: { args: NoArgs; result: DeviceInfo };

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
  /** band 0..7 (mic EQ bands 1..8), gain -12..12 dB in 0.5 steps. */
  set_mic_eq_band: { args: { band: number; gain: number }; result: AppState };
  /** Writes all 8 mic EQ bands and turns mic EQ on. Allowed in Direct Mode and Scout. */
  apply_mic_eq_curve: { args: { bands: number[] }; result: AppState };
  list_mic_eq_curves: { args: NoArgs; result: MicEqCurve[] };
  /** Saves the active output's current mic EQ bands (replaces a curve with the same name). */
  save_mic_eq_curve: { args: { name: string }; result: AppState };
  delete_mic_eq_curve: { args: { name: string }; result: AppState };
  set_mic_aec: { args: { enabled: boolean }; result: AppState };
  set_mic_smart_volume: { args: { enabled: boolean }; result: AppState };

  // Device
  set_dac_filter: { args: { filter: DacFilter }; result: AppState };
  set_lighting_enabled: { args: { enabled: boolean }; result: AppState };
  /** color "rrggbb"; rejects invalidValue otherwise. */
  set_lighting_color: { args: { color: string }; result: AppState };
  /** Manual override; the headphone jack still switches the output on plug / unplug. */
  set_output: { args: { output: Output }; result: AppState };
  set_dolby_decoder: { args: { mode: DolbyMode }; result: AppState };
  set_direct_mode: { args: { enabled: boolean }; result: AppState };

  // Mixer (USB audio controls; works in Direct Mode and while the HID side is disconnected)
  /** volume 0..1, both channels. Rejects invalidValue, mixerUnavailable. */
  set_mixer_volume: { args: { scope: MixerScope; volume: number }; result: AppState };
  /** Rejects mixerUnavailable when the scope has no mute. */
  set_mixer_mute: { args: { scope: MixerScope; mute: boolean }; result: AppState };
  /** id from `mixer.recordingSource.options`; rejects invalidValue for another id. */
  set_mixer_source: { args: { id: number }; result: AppState };

  // Presets
  list_presets: { args: NoArgs; result: string[] };
  save_preset: { args: { name: string }; result: AppState };
  apply_preset: { args: { name: string }; result: AppState };
  rename_preset: { args: { from: string; to: string }; result: AppState };
  delete_preset: { args: { name: string }; result: AppState };
  /** Opens the native Save dialog and writes the profile there as JSON. Resolves with the path written, or null if cancelled. */
  export_preset: { args: { name: string }; result: string | null };
  /** Saves the profile file `text` under `name` (not applied). Rejects invalidValue (bad content), presetInvalidName (bad or taken name). */
  import_preset: { args: { text: string; name: string }; result: AppState };
  /** Maps an AutoEq / Equalizer APO text to bands and pre-amp; pure, rejects invalidValue. Apply with `apply_eq_curve`. */
  import_eq: { args: { text: string }; result: EqImport };
  /** Profile to apply when the output becomes `output`; null clears the rule. Rejects presetNotFound. */
  set_auto_switch: { args: { output: Output; profile: string | null }; result: AppState };

  // App
  set_launch_at_login: { args: { enabled: boolean }; result: AppState };
  /** Turns the global hotkeys on / off and sets the accelerators ("" = that action's default). Rejects invalidValue (bad or duplicate accelerator), storage. A shortcut the OS refuses is reported in `hotkeys.*.error`, not as a rejection. */
  set_hotkeys: {
    args: { enabled: boolean; toggleDirectMode: string; nextProfile: string; toggleSbx: string };
    result: AppState;
  };
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
      eqBands: new Array<number>(8).fill(0),
      aec: false,
      smartVolume: false,
    },
    dacFilter: null,
    dolbyDecoder: null,
    lighting: { enabled: false, color: 'ffffff' },
    presets: [],
    activePreset: null,
    eqCurves: [],
    micEqCurves: [],
    autoSwitch: { headphones: null, speakers: null },
    launchAtLogin: false,
    hotkeys: {
      enabled: false,
      toggleDirectMode: { accelerator: '', error: null },
      nextProfile: { accelerator: '', error: null },
      toggleSbx: { accelerator: '', error: null },
    },
    mixer: null,
  };
}
