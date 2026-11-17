// In-memory stand-in for the Rust backend, used automatically when the UI runs
// outside Tauri (`npm run dev` in a browser). It follows app/API.md: same
// commands, same errors, same events, same Scout / Direct Mode rules. It is
// not a simulation of the device.
//
// Dev handle in the browser console: `__g6mock.switchOutput()`,
// `.disconnect()`, `.connect()`, `.pressScout()`, `.pressSbx()`,
// `.failNext('deviceTimeout')`, `.takeShortcut('Ctrl+Alt+X')` (the OS refuses that hotkey).
// `.turnKnob(0.3)` (the volume knob moves the playback volume).
// `?mock=disconnected` starts disconnected, `?mock=nomixer` has no mixer (as on Windows).

import type {
  ApiError,
  AppState,
  AutoSwitch,
  Backend,
  DacFilter,
  DolbyMode,
  EffectName,
  Effects,
  ErrorCode,
  Eq,
  EqCurve,
  EqImport,
  HotkeyBinding,
  Hotkeys,
  Lighting,
  Mic,
  Mixer,
  MixerLevel,
  MixerScope,
  MicBoost,
  MicEqCurve,
  Output,
  SmartVolumeMode,
} from './api';
import { MAX_EQ_IMPORT_BYTES, MAX_PRESET_FILE_BYTES } from './api';
import { MAX_CURVE_NAME, RESERVED_NAMES } from './eqCurves';
import { defaultAccelerator, normalizeAccelerator } from './hotkeys';
import { MIC_BUILTIN_CURVES, MIC_RESERVED_NAMES } from './micEqCurves';

const SCOUT_CURVE = [2, 3, 3, -2, -2, 0, 3, 4, 3, 2];
const SCOUT_SMART_VOLUME = 0.9;
const LATENCY_MS = 25;
const MAX_CURVES = 32;

interface Profile {
  eq: Eq;
  effects: Effects;
  mic: Omit<Mic, 'boost'>;
}

interface PresetEntry {
  headphones: { eq: Eq; effects: Effects };
  speakers: { eq: Eq; effects: Effects };
  sbx: boolean;
}

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

function err(code: ErrorCode, message: string): ApiError {
  return { code, message };
}

function profile(
  bands: number[],
  preamp: number,
  fx: Partial<Record<EffectName, { enabled: boolean; level: number }>>,
  micEq: boolean,
  micBands: number[] = new Array<number>(8).fill(0),
): Profile {
  const f = (n: EffectName) => fx[n] ?? { enabled: false, level: 0.5 };
  return {
    eq: { enabled: false, bands, preamp },
    effects: {
      surround: { ...f('surround'), distance: 80 },
      crystalizer: { ...f('crystalizer') },
      bass: { ...f('bass') },
      dialog: { ...f('dialog') },
      smartVolume: { ...f('smartVolume'), mode: 'normal' },
    },
    mic: { voiceClarity: true, noiseReduction: 0, eqEnabled: micEq, eqBands: micBands, aec: false, smartVolume: false },
  };
}

/** The driver's dB for a scalar (made-up curves with the G6's real ranges: -64..0, -64..9, -48..9 dB). */
const MIXER_DB: Record<MixerScope, (v: number) => number> = {
  playback: (v) => Math.round((-64 + 64 * v) * 100) / 100,
  monitoring: (v) => Math.round((-64 + 73 * v) * 100) / 100,
  recording: (v) => Math.round((-48 + 57 * v) * 100) / 100,
};

function mockMixer(): Mixer {
  const level = (scope: MixerScope, volume: number): MixerLevel => ({ volume, volumeDb: MIXER_DB[scope](volume), mute: false });
  return {
    playback: level('playback', 0.56),
    monitoring: level('monitoring', 0.77),
    recording: level('recording', 0.71),
    recordingSource: {
      current: 1,
      options: [
        { id: 1, name: 'Line In' },
        { id: 2, name: 'External Mic' },
        { id: 3, name: 'S/PDIF In' },
        { id: 4, name: 'What U Hear' },
      ],
    },
  };
}

class MockBackend implements Backend {
  private rev = 1;
  private listeners = new Map<string, Set<(p: unknown) => void>>();
  private connection: AppState['connection'] = 'connected';
  private firmware = '2.1.250903.1324';
  private output: Output = 'headphones';
  private profiles: Record<Output, Profile> = {
    headphones: profile(
      [0, 0, 0, 1, 0, 1.5, 1, 1.5, 1, 0.5],
      0,
      { surround: { enabled: true, level: 0.12 } },
      true,
      [...MIC_BUILTIN_CURVES[0].bands],
    ),
    speakers: profile(
      [-1, 0, 0.5, 1, 1, 1.5, 1, 1.5, 1, 0.5],
      -2.5,
      {
        surround: { enabled: true, level: 0.12 },
        crystalizer: { enabled: true, level: 0.5 },
        smartVolume: { enabled: false, level: 0.74 },
      },
      false,
    ),
  };
  private directMode = false;
  private sbx = true;
  private scout = false;
  /** Pre-amp while Scout is on: 0 dB when it turns on; the real one comes back when it turns off. */
  private scoutPreamp = 0;
  private micBoost: MicBoost = 30;
  private dacFilter: DacFilter = 'fast-min';
  private dolby: DolbyMode = 'normal';
  private lighting: Lighting = { enabled: true, color: '00aaff' };
  private presets = new Map<string, PresetEntry>();
  private eqCurves: EqCurve[] = [];
  private micEqCurves: MicEqCurve[] = [];
  private activePreset: string | null = 'Default';
  private autoSwitch: AutoSwitch = { headphones: null, speakers: null };
  private launch = true;
  private hotkeys = { enabled: false, toggleDirectMode: defaultAccelerator('D'), nextProfile: defaultAccelerator('P'), toggleSbx: defaultAccelerator('S') };
  /** Accelerators the "OS" refuses to register (dev handle `takeShortcut`). */
  private taken = new Set<string>();
  private failCode: ErrorCode | null = null;
  /** The G6's USB audio controls; null hides the tab (`?mock=nomixer`). */
  private mixer: Mixer | null = mockMixer();

  constructor(startDisconnected: boolean, noMixer = false) {
    if (startDisconnected) this.connection = 'disconnected';
    if (noMixer) this.mixer = null;
    for (const n of ['Default', 'Late night']) this.presets.set(n, this.capture());
  }

  // --- Backend -----------------------------------------------------------

  listen(name: string, handler: (payload: unknown) => void): () => void {
    let set = this.listeners.get(name);
    if (!set) this.listeners.set(name, (set = new Set()));
    set.add(handler);
    return () => set.delete(handler);
  }

  async invoke(cmd: string, args: Record<string, unknown>): Promise<unknown> {
    await new Promise((r) => setTimeout(r, LATENCY_MS));
    if (cmd === 'get_state' || cmd === 'get_device_info' || cmd === 'hide_panel' || cmd === 'set_app_theme' || cmd === 'list_presets' || cmd === 'list_eq_curves' || cmd === 'list_mic_eq_curves') {
      return this.read(cmd);
    }
    if (this.failCode) {
      const code = this.failCode;
      this.failCode = null;
      throw err(code, `Mock failure: ${code}`);
    }
    if (cmd === 'refresh_state') return this.snapshot();
    if (cmd === 'import_eq') return mockImportEq(String(args.text ?? ''));
    if (cmd === 'export_preset') return this.exportPreset(String(args.name ?? ''));
    if (cmd.startsWith('set_mixer_')) return this.writeMixer(cmd, args);
    if (this.connection === 'disconnected' && cmd !== 'delete_preset' &&
      cmd !== 'import_preset' &&
      cmd !== 'rename_preset' &&
      cmd !== 'delete_eq_curve' &&
      cmd !== 'delete_mic_eq_curve' &&
      cmd !== 'set_auto_switch' &&
      cmd !== 'set_launch_at_login' &&
      cmd !== 'set_hotkeys' &&
      !(cmd === 'set_direct_mode' && !args.enabled && !this.directMode)) {
      throw err('notConnected', 'The G6 is not connected.');
    }
    this.write(cmd, args);
    const s = this.snapshot();
    this.emit('state-changed', s);
    return s;
  }

  // --- Dev handle --------------------------------------------------------

  /** The headphone jack. Applies the output's auto-switch profile, as the backend does. */
  switchOutput(): void {
    this.output = this.output === 'headphones' ? 'speakers' : 'headphones';
    this.emit('output-changed', { output: this.output });
    const name = this.autoSwitch[this.output];
    const pr = name === null ? undefined : this.presets.get(name);
    if (name !== null && pr && !this.directMode) {
      const src = clone(pr[this.output]);
      const p = this.profiles[this.output];
      p.eq = src.eq;
      p.effects = src.effects;
      this.sbx = pr.sbx;
      this.scout = false;
      this.activePreset = name;
    }
    this.push();
  }

  disconnect(): void {
    this.connection = 'disconnected';
    this.emit('connection-changed', { connection: this.connection, firmware: this.firmware });
    this.push();
  }

  connect(): void {
    this.connection = 'connected';
    this.emit('connection-changed', { connection: this.connection, firmware: this.firmware });
    this.push();
  }

  /** The Scout button on the device. */
  pressScout(): void {
    this.setScout(!this.scout);
    this.push();
  }

  /** The SBX button on the device. */
  pressSbx(): void {
    this.sbx = true;
    this.scout = false;
    this.push();
  }

  /** Make the OS refuse a hotkey, as when another app uses it. Takes effect on the next `set_hotkeys`. */
  takeShortcut(accelerator: string): void {
    this.taken.add(normalizeAccelerator(accelerator));
  }

  /** The volume knob (or another app) moves the playback volume. */
  turnKnob(volume: number): void {
    if (!this.mixer?.playback) return;
    this.mixer.playback.volume = volume;
    this.mixer.playback.volumeDb = MIXER_DB.playback(volume);
    this.push();
  }

  failNext(code: ErrorCode = 'deviceTimeout'): void {
    this.failCode = code;
  }

  // --- Internals ---------------------------------------------------------

  private emit(name: string, payload: unknown): void {
    this.listeners.get(name)?.forEach((h) => h(payload));
  }

  private push(): void {
    this.emit('state-changed', this.snapshot());
  }

  private read(cmd: string): unknown {
    if (cmd === 'get_state') return this.snapshot();
    if (cmd === 'get_device_info') {
      if (this.connection === 'disconnected') {
        return { firmware: this.firmware, serial: null, dspVersion: null, hardwareId: null };
      }
      return { firmware: this.firmware, serial: 'ef67740000000000', dspVersion: '30011000', hardwareId: '97000000' };
    }
    if (cmd === 'list_presets') return [...this.presets.keys()].sort();
    if (cmd === 'list_eq_curves') return clone(this.eqCurves);
    if (cmd === 'list_mic_eq_curves') return clone(this.micEqCurves);
    return null;
  }

  private snapshot(): AppState {
    const p = clone(this.profiles[this.output]);
    let sbx = this.sbx;
    let scout = this.scout;
    const eq = p.eq;
    // Scout owns the EQ curve and Smart Volume; SBX is off while it is on. The device also
    // sets the pre-amp to 0 dB (still writable) and restores it when Scout turns off.
    if (scout) {
      eq.enabled = true;
      eq.preamp = this.scoutPreamp;
      eq.bands = [...SCOUT_CURVE];
      p.effects.smartVolume.enabled = true;
      p.effects.smartVolume.level = SCOUT_SMART_VOLUME;
      sbx = false;
    }
    // Direct Mode turns EQ, SBX and Scout off on the device.
    if (this.directMode) {
      eq.enabled = false;
      sbx = false;
      scout = false;
    }
    return {
      revision: ++this.rev,
      connection: this.connection,
      firmware: this.firmware,
      output: this.output,
      directMode: this.directMode,
      sbx,
      scout,
      eq,
      effects: p.effects,
      mic: { boost: this.micBoost, ...p.mic },
      dacFilter: this.dacFilter,
      dolbyDecoder: this.dolby,
      lighting: { ...this.lighting },
      presets: [...this.presets.keys()].sort(),
      activePreset: this.activePreset,
      eqCurves: clone(this.eqCurves),
      micEqCurves: clone(this.micEqCurves),
      autoSwitch: { ...this.autoSwitch },
      launchAtLogin: this.launch,
      hotkeys: this.hotkeyState(),
      mixer: clone(this.mixer),
    };
  }

  /** The mixer commands: validated like the backend, announced like any change. */
  private writeMixer(cmd: string, a: Record<string, unknown>): AppState {
    const m = this.mixer;
    if (!m) throw err('mixerUnavailable', 'The G6 audio device is not available.');
    const level = (): MixerLevel => {
      const l = m[a.scope as MixerScope];
      if (!l || typeof l !== 'object') throw err('mixerUnavailable', `The G6 offers no ${String(a.scope)} control.`);
      return l;
    };
    switch (cmd) {
      case 'set_mixer_volume': {
        const v = Number(a.volume);
        if (!Number.isFinite(v) || v < 0 || v > 1) throw err('invalidValue', `Volume ${String(a.volume)} is outside 0..1.`);
        const l = level();
        l.volume = v;
        l.volumeDb = MIXER_DB[a.scope as MixerScope](v);
        break;
      }
      case 'set_mixer_mute': {
        const l = level();
        if (l.mute === null) throw err('mixerUnavailable', 'No mute control.');
        l.mute = Boolean(a.mute);
        break;
      }
      case 'set_mixer_source': {
        const s = m.recordingSource;
        if (!s) throw err('mixerUnavailable', 'No recording source selector.');
        if (!s.options.some((o) => o.id === a.id)) throw err('invalidValue', `Unknown recording source ${String(a.id)}.`);
        s.current = a.id as number;
        break;
      }
      default:
        throw err('internal', `Unknown command ${cmd}.`);
    }
    const s = this.snapshot();
    this.emit('state-changed', s);
    return s;
  }

  private hotkeyState(): Hotkeys {
    const h = this.hotkeys;
    const binding = (accelerator: string): HotkeyBinding => ({
      accelerator,
      error: h.enabled && this.taken.has(accelerator)
        ? `${accelerator} could not be registered (another app may be using it).`
        : null,
    });
    return {
      enabled: h.enabled,
      toggleDirectMode: binding(h.toggleDirectMode),
      nextProfile: binding(h.nextProfile),
      toggleSbx: binding(h.toggleSbx),
    };
  }

  /** Browser stand-in for the native Save dialog: downloads the profile file. Resolves with its file name. */
  private exportPreset(name: string): string {
    const pr = this.presets.get(name);
    if (!pr) throw err('presetNotFound', `No profile named "${name}".`);
    const text = presetFileText(pr);
    const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = `${name}.json`;
    link.click();
    URL.revokeObjectURL(url);
    return `${name}.json`;
  }

  private capture(): PresetEntry {
    const one = (o: Output) => {
      const p = clone(this.profiles[o]);
      return { eq: p.eq, effects: p.effects };
    };
    return { headphones: one('headphones'), speakers: one('speakers'), sbx: this.sbx };
  }

  private guardPlayback(): void {
    if (this.directMode) throw err('directModeActive', 'Direct Mode is on.');
  }

  private guardEq(): void {
    this.guardPlayback();
    if (this.scout) throw err('scoutActive', 'Scout Mode is on.');
  }

  private setScout(on: boolean): void {
    if (on && !this.scout) this.scoutPreamp = 0;
    this.scout = on;
    if (on) this.sbx = false;
  }

  /**
   * Like `Feature::validate`: rounded to the step (half away from zero) first,
   * then range-checked. `exact` (indices, enum-like values) rejects off-step.
   */
  private num(v: unknown, lo: number, hi: number, step: number, exact = false): number {
    const n = Number(v);
    const k = n / step;
    const rk = Math.sign(k) * Math.round(Math.abs(k));
    const r = Math.round(rk * step * 1e6) / 1e6 || 0;
    if (!Number.isFinite(n) || r < lo || r > hi || (exact && Math.abs(k - rk) > 1e-6)) {
      throw err('invalidValue', `Value ${String(v)} is out of range.`);
    }
    return r;
  }

  private write(cmd: string, a: Record<string, unknown>): void {
    const p = this.profiles[this.output];
    const effect = (): keyof Effects => {
      const e = a.effect as EffectName;
      if (!(e in p.effects)) throw err('invalidValue', `Unknown effect ${String(e)}.`);
      return e;
    };
    switch (cmd) {
      case 'set_eq_enabled':
        this.guardEq();
        p.eq.enabled = Boolean(a.enabled);
        break;
      case 'set_eq_band': {
        this.guardEq();
        const band = this.num(a.band, 0, 9, 1, true);
        p.eq.bands[band] = this.num(a.gain, -12, 12, 0.5);
        break;
      }
      case 'set_eq_bands': {
        this.guardEq();
        const g = a.gains as number[];
        if (!Array.isArray(g) || g.length !== 10) throw err('invalidValue', 'Need exactly 10 gains.');
        p.eq.bands = g.map((v) => this.num(v, -12, 12, 0.5));
        break;
      }
      case 'set_eq_preamp':
        // Not Scout-owned: only Direct Mode blocks it. With Scout on it is the device's 0 dB value.
        this.guardPlayback();
        if (this.scout) this.scoutPreamp = this.num(a.db, -6, 6, 0.5);
        else p.eq.preamp = this.num(a.db, -6, 6, 0.5);
        break;
      case 'apply_eq_curve': {
        this.guardEq();
        const g = a.bands as number[];
        if (!Array.isArray(g) || g.length !== 10) throw err('invalidValue', 'Need exactly 10 gains.');
        const bands = g.map((v) => this.num(v, -12, 12, 0.5));
        p.eq.preamp = this.num(a.preamp, -6, 6, 0.5);
        p.eq.bands = bands;
        p.eq.enabled = true;
        break;
      }
      case 'save_eq_curve': {
        this.guardEq();
        const name = String(a.name ?? '').trim();
        if (!name) throw err('invalidValue', 'Give the curve a name.');
        if ([...name].length > MAX_CURVE_NAME) {
          throw err('invalidValue', `Curve names are at most ${MAX_CURVE_NAME} characters.`);
        }
        if (RESERVED_NAMES.some((r) => r.toLowerCase() === name.toLowerCase())) {
          throw err('invalidValue', `"${name}" is a built-in curve name.`);
        }
        const curve: EqCurve = { name, bands: [...p.eq.bands], preamp: p.eq.preamp };
        const i = this.eqCurves.findIndex((c) => c.name.toLowerCase() === name.toLowerCase());
        if (i >= 0) this.eqCurves[i] = curve;
        else if (this.eqCurves.length >= MAX_CURVES) {
          throw err('invalidValue', `You can keep at most ${MAX_CURVES} saved curves.`);
        } else this.eqCurves.push(curve);
        break;
      }
      case 'delete_eq_curve': {
        const name = String(a.name ?? '').trim().toLowerCase();
        const i = this.eqCurves.findIndex((c) => c.name.toLowerCase() === name);
        if (i < 0) throw err('curveNotFound', `No saved curve named "${String(a.name)}".`);
        this.eqCurves.splice(i, 1);
        break;
      }
      case 'set_sbx':
        this.guardPlayback();
        this.sbx = Boolean(a.enabled);
        if (this.sbx) this.scout = false;
        break;
      case 'set_scout':
        this.guardPlayback();
        this.setScout(Boolean(a.enabled));
        break;
      case 'set_effect_enabled': {
        this.guardPlayback();
        const e = effect();
        if (e === 'smartVolume' && this.scout) throw err('scoutActive', 'Scout Mode is on.');
        p.effects[e].enabled = Boolean(a.enabled);
        break;
      }
      case 'set_effect_level': {
        this.guardPlayback();
        const e = effect();
        if (e === 'smartVolume' && this.scout) throw err('scoutActive', 'Scout Mode is on.');
        p.effects[e].level = this.num(a.level, 0, 1, 0.01);
        break;
      }
      case 'set_surround_distance':
        this.guardPlayback();
        p.effects.surround.distance = this.num(a.distance, 10, 300, 0.5);
        break;
      case 'set_smart_volume_mode':
        this.guardPlayback();
        p.effects.smartVolume.mode = a.mode as SmartVolumeMode;
        break;
      case 'set_mic_boost':
        this.micBoost = this.num(a.db, 0, 30, 10, true) as MicBoost;
        break;
      case 'set_voice_clarity':
        p.mic.voiceClarity = Boolean(a.enabled);
        break;
      case 'set_noise_reduction':
        p.mic.noiseReduction = this.num(a.level, 0, 1, 0.01);
        break;
      case 'set_mic_eq_enabled':
        p.mic.eqEnabled = Boolean(a.enabled);
        break;
      case 'set_mic_eq_band': {
        const b = Number(a.band);
        if (!Number.isInteger(b) || b < 0 || b > 7) throw err('invalidValue', `Mic EQ band ${String(a.band)} does not exist (0..7).`);
        p.mic.eqBands[b] = this.num(a.gain, -12, 12, 0.5);
        break;
      }
      case 'apply_mic_eq_curve': {
        const g = a.bands as number[];
        if (!Array.isArray(g) || g.length !== 8) throw err('invalidValue', 'Need exactly 8 gains.');
        p.mic.eqBands = g.map((v) => this.num(v, -12, 12, 0.5));
        p.mic.eqEnabled = true;
        break;
      }
      case 'save_mic_eq_curve': {
        const name = String(a.name ?? '').trim();
        if (!name) throw err('invalidValue', 'Give the curve a name.');
        if ([...name].length > MAX_CURVE_NAME) {
          throw err('invalidValue', `Curve names are at most ${MAX_CURVE_NAME} characters.`);
        }
        if (MIC_RESERVED_NAMES.some((r) => r.toLowerCase() === name.toLowerCase())) {
          throw err('invalidValue', `"${name}" is a built-in curve name.`);
        }
        const curve: MicEqCurve = { name, bands: [...p.mic.eqBands] };
        const i = this.micEqCurves.findIndex((c) => c.name.toLowerCase() === name.toLowerCase());
        if (i >= 0) this.micEqCurves[i] = curve;
        else if (this.micEqCurves.length >= MAX_CURVES) {
          throw err('invalidValue', `You can keep at most ${MAX_CURVES} saved curves.`);
        } else this.micEqCurves.push(curve);
        break;
      }
      case 'delete_mic_eq_curve': {
        const name = String(a.name ?? '').trim().toLowerCase();
        const i = this.micEqCurves.findIndex((c) => c.name.toLowerCase() === name);
        if (i < 0) throw err('curveNotFound', `No saved curve named "${String(a.name)}".`);
        this.micEqCurves.splice(i, 1);
        break;
      }
      case 'set_mic_aec':
        p.mic.aec = Boolean(a.enabled);
        break;
      case 'set_mic_smart_volume':
        p.mic.smartVolume = Boolean(a.enabled);
        break;
      case 'set_dac_filter':
        this.dacFilter = a.filter as DacFilter;
        break;
      case 'set_lighting_enabled':
        this.lighting.enabled = Boolean(a.enabled);
        break;
      case 'set_lighting_color': {
        const c = typeof a.color === 'string' ? a.color.replace(/^#/, '').toLowerCase() : '';
        if (!/^[0-9a-f]{6}$/.test(c)) {
          throw err('invalidValue', `Lighting color must be rrggbb hex, not "${String(a.color)}".`);
        }
        this.lighting.color = c;
        break;
      }
      case 'set_output': {
        const o = a.output as Output;
        if (o !== this.output) {
          this.output = o;
          this.emit('output-changed', { output: o });
        }
        break;
      }
      case 'set_dolby_decoder':
        this.dolby = a.mode as DolbyMode;
        break;
      case 'set_direct_mode':
        this.directMode = Boolean(a.enabled);
        break;
      case 'save_preset': {
        this.guardPlayback();
        const name = String(a.name ?? '');
        if (!/^[A-Za-z0-9_\- ][A-Za-z0-9_\-. ]{0,63}$/.test(name)) {
          throw err('presetInvalidName', `"${name}" is not a valid profile name.`);
        }
        this.presets.set(name, this.capture());
        this.activePreset = name;
        break;
      }
      case 'apply_preset': {
        this.guardPlayback();
        const name = String(a.name ?? '');
        const pr = this.presets.get(name);
        if (!pr) throw err('presetNotFound', `No profile named "${name}".`);
        const src = clone(pr[this.output]);
        p.eq = src.eq;
        p.effects = src.effects;
        this.sbx = pr.sbx;
        this.scout = false;
        this.activePreset = name;
        break;
      }
      case 'rename_preset': {
        const from = String(a.from ?? '');
        const to = String(a.to ?? '');
        const pr = this.presets.get(from);
        if (!pr) throw err('presetNotFound', `No profile named "${from}".`);
        if (!/^[A-Za-z0-9_\- ][A-Za-z0-9_\-. ]{0,63}$/.test(to)) {
          throw err('presetInvalidName', `"${to}" is not a valid profile name.`);
        }
        const clash = [...this.presets.keys()].some((n) => n !== from && n.toLowerCase() === to.toLowerCase());
        if (clash) throw err('presetInvalidName', `A profile named "${to}" already exists.`);
        this.presets.delete(from);
        this.presets.set(to, pr);
        if (this.activePreset === from) this.activePreset = to;
        for (const o of ['headphones', 'speakers'] as const) {
          if (this.autoSwitch[o] === from) this.autoSwitch[o] = to;
        }
        break;
      }
      case 'import_preset': {
        const name = String(a.name ?? '');
        const entry = parsePresetFile(String(a.text ?? ''));
        if (!/^[A-Za-z0-9_\- ][A-Za-z0-9_\-. ]{0,63}$/.test(name)) {
          throw err('presetInvalidName', `"${name}" is not a valid profile name.`);
        }
        if ([...this.presets.keys()].some((n) => n.toLowerCase() === name.toLowerCase())) {
          throw err('presetInvalidName', `A profile named "${name}" already exists.`);
        }
        this.presets.set(name, entry);
        break;
      }
      case 'delete_preset': {
        const name = String(a.name ?? '');
        if (!this.presets.delete(name)) throw err('presetNotFound', `No profile named "${name}".`);
        if (this.activePreset === name) this.activePreset = null;
        for (const o of ['headphones', 'speakers'] as const) {
          if (this.autoSwitch[o] === name) this.autoSwitch[o] = null;
        }
        break;
      }
      case 'set_auto_switch': {
        const o = a.output as Output;
        const name = a.profile === null ? null : String(a.profile);
        if (name !== null && !this.presets.has(name)) throw err('presetNotFound', `No profile named "${name}".`);
        this.autoSwitch[o] = name;
        break;
      }
      case 'set_launch_at_login':
        this.launch = Boolean(a.enabled);
        break;
      case 'set_hotkeys': {
        const defaults = { toggleDirectMode: defaultAccelerator('D'), nextProfile: defaultAccelerator('P'), toggleSbx: defaultAccelerator('S') };
        const next = { enabled: Boolean(a.enabled), ...defaults };
        for (const k of ['toggleDirectMode', 'nextProfile', 'toggleSbx'] as const) {
          const raw = String(a[k] ?? '').trim();
          try {
            if (raw) next[k] = normalizeAccelerator(raw);
          } catch (e) {
            throw err('invalidValue', e instanceof Error ? e.message : String(e));
          }
        }
        const list = [next.toggleDirectMode, next.nextProfile, next.toggleSbx];
        if (new Set(list).size !== list.length) {
          throw err('invalidValue', 'Two actions cannot use the same shortcut.');
        }
        this.hotkeys = next;
        break;
      }
      default:
        throw err('notImplemented', `Unknown command ${cmd}.`);
    }
  }
}

// --- Profile files and EQ import (reduced ports of the Rust side) -----------

const BAND_KEYS = ['31', '62', '125', '250', '500', '1k', '2k', '4k', '8k', '16k'].map((b) => `eq_${b}`);
const EFFECT_KEYS: [EffectName, string][] = [
  ['surround', 'surround'],
  ['crystalizer', 'crystalizer'],
  ['bass', 'bass'],
  ['dialog', 'dialog'],
  ['smartVolume', 'smartvol'],
];

/** The profile file of a stored profile: feature names to device values, like `g6-core`'s preset JSON. */
function presetFileText(pr: PresetEntry): string {
  const file: Record<string, Record<string, number | boolean>> = {};
  for (const o of ['headphones', 'speakers'] as const) {
    const { eq, effects } = pr[o];
    const p: Record<string, number | boolean> = {};
    if (o === 'headphones') p.sbx = pr.sbx;
    p.eq = eq.enabled ? 1 : 0;
    p.eq_preamp = eq.preamp;
    BAND_KEYS.forEach((k, i) => (p[k] = eq.bands[i]));
    for (const [name, key] of EFFECT_KEYS) {
      p[key] = effects[name].enabled ? 1 : 0;
      p[`${key}_level`] = effects[name].level;
    }
    p.surround_distance = effects.surround.distance;
    file[o] = p;
  }
  return JSON.stringify(file, null, 2) + '\n';
}

/** Check a profile file the way `Preset::from_json` does (allow-list and ranges), reduced to what the mock stores. */
function parsePresetFile(text: string): PresetEntry {
  if (new Blob([text]).size > MAX_PRESET_FILE_BYTES) {
    throw err('invalidValue', `invalid preset: profile file is larger than ${MAX_PRESET_FILE_BYTES / 1024} KiB`);
  }
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (e) {
    throw err('invalidValue', `invalid preset: not a profile file: ${String(e)}`);
  }
  const bad = (m: string) => err('invalidValue', `invalid preset: ${m}`);
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw bad('not a profile file');
  const file = raw as Record<string, Record<string, unknown> | undefined>;
  if (!file.headphones && !file.speakers) throw bad("the file has no 'headphones' or 'speakers' profile");
  const flat = (): Profile['eq'] => ({ enabled: false, bands: new Array<number>(10).fill(0), preamp: 0 });
  const blank = () => clone(profile(new Array<number>(10).fill(0), 0, {}, false)).effects;
  const entry: PresetEntry = {
    headphones: { eq: flat(), effects: blank() },
    speakers: { eq: flat(), effects: blank() },
    sbx: false,
  };
  const levels: Record<string, [number, number]> = { eq_preamp: [-6, 6], surround_distance: [10, 300] };
  for (const o of ['headphones', 'speakers'] as const) {
    const p = file[o];
    if (p === undefined) continue;
    if (typeof p !== 'object' || p === null) throw bad(`${o}: not a profile`);
    const target = entry[o];
    for (const [k, v] of Object.entries(p)) {
      if (k === 'sbx' || k === 'scout') {
        if (typeof v !== 'boolean') throw bad(`${o}: ${k} must be true or false`);
        if (k === 'sbx' && o === 'headphones') entry.sbx = v;
        continue;
      }
      if (typeof v !== 'number' || !Number.isFinite(v)) throw bad(`${o}: ${k} is not a number`);
      const band = BAND_KEYS.indexOf(k);
      const fx = EFFECT_KEYS.find(([, key]) => k === key || k === `${key}_level`);
      const [lo, hi] = band >= 0 ? [-12, 12] : (levels[k] ?? (k === 'smartvol_mode' ? [0, 2] : [0, 1]));
      const known = k === 'eq' || k === 'smartvol_mode' || k in levels || band >= 0 || fx;
      if (!known) throw bad(`${o}: unknown feature '${k}'`);
      if (v < lo || v > hi) throw bad(`${o}: ${k} ${v} is outside ${lo}..${hi}`);
      if (k === 'eq') target.eq.enabled = v === 1;
      else if (k === 'eq_preamp') target.eq.preamp = v;
      else if (band >= 0) target.eq.bands[band] = v;
      else if (k === 'surround_distance') target.effects.surround.distance = v;
      else if (fx) {
        const e = target.effects[fx[0]];
        if (k === fx[1]) e.enabled = v === 1;
        else e.level = v;
      }
    }
  }
  return entry;
}

const BAND_HZ = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const FS = 48000;

/** RBJ biquad response in dB at `hz` (peaking, low shelf, high shelf). */
function biquadDb(kind: 'pk' | 'ls' | 'hs', fc: number, gain: number, q: number, hz: number): number {
  const a = 10 ** (gain / 40);
  const w0 = (2 * Math.PI * fc) / FS;
  const cos = Math.cos(w0);
  const alpha = Math.sin(w0) / (2 * q);
  const sa = 2 * Math.sqrt(a) * alpha;
  let b: number[];
  let d: number[];
  if (kind === 'pk') {
    b = [1 + alpha * a, -2 * cos, 1 - alpha * a];
    d = [1 + alpha / a, -2 * cos, 1 - alpha / a];
  } else if (kind === 'ls') {
    b = [a * (a + 1 - (a - 1) * cos + sa), 2 * a * (a - 1 - (a + 1) * cos), a * (a + 1 - (a - 1) * cos - sa)];
    d = [a + 1 + (a - 1) * cos + sa, -2 * (a - 1 + (a + 1) * cos), a + 1 + (a - 1) * cos - sa];
  } else {
    b = [a * (a + 1 + (a - 1) * cos + sa), -2 * a * (a - 1 + (a + 1) * cos), a * (a + 1 + (a - 1) * cos - sa)];
    d = [a + 1 - (a - 1) * cos + sa, 2 * (a - 1 - (a + 1) * cos), a + 1 - (a - 1) * cos - sa];
  }
  const w = (2 * Math.PI * hz) / FS;
  const mag2 = (c: number[]) => {
    const re = c[0] + c[1] * Math.cos(w) + c[2] * Math.cos(2 * w);
    const im = -(c[1] * Math.sin(w) + c[2] * Math.sin(2 * w));
    return re * re + im * im;
  };
  return 10 * Math.log10(mag2(b) / mag2(d));
}

/**
 * `import_eq` for the browser mock: Preamp, `Filter N: ON PK / LSC / HSC` and `GraphicEQ:` lines
 * (the Rust parser in `g6-core::eq_import` is the reference and also reports ignored lines).
 */
function mockImportEq(text: string): EqImport {
  if (new Blob([text]).size > MAX_EQ_IMPORT_BYTES) {
    throw err('invalidValue', `EQ text is larger than ${MAX_EQ_IMPORT_BYTES / 1024} KiB`);
  }
  const filters: { kind: 'pk' | 'ls' | 'hs'; fc: number; gain: number; q: number }[] = [];
  let graphic: [number, number][] = [];
  let preamp = 0;
  let seen = false;
  for (const line of text.split(/\r?\n/)) {
    const m = /^\s*(preamp|filter\b[^:]*|graphiceq)\s*:(.*)$/i.exec(line.split('#')[0]);
    if (!m) continue;
    seen = true;
    const head = m[1].toLowerCase();
    const rest = m[2].trim();
    if (head === 'preamp') {
      const n = parseFloat(rest);
      if (Number.isNaN(n)) throw err('invalidValue', `line: pre-amp '${rest}' is not a number of dB`);
      preamp += n;
    } else if (head === 'graphiceq') {
      graphic = rest.split(';').map((p) => p.trim()).filter(Boolean).map((p) => {
        const [f, g] = p.split(/\s+/).map(Number);
        return [f, g] as [number, number];
      }).sort((x, y) => x[0] - y[0]);
    } else {
      const t = rest.split(/\s+/);
      if (t[0]?.toLowerCase() !== 'on') continue;
      const kind = { pk: 'pk', peq: 'pk', ls: 'ls', lsc: 'ls', hs: 'hs', hsc: 'hs' }[t[1]?.toLowerCase()] as 'pk' | 'ls' | 'hs' | undefined;
      const num = (key: string) => {
        const i = t.findIndex((x) => x.toLowerCase() === key);
        return i >= 0 ? parseFloat(t[i + 1]) : NaN;
      };
      const fc = num('fc');
      const gain = num('gain');
      const q = num('q');
      if (!kind || Number.isNaN(fc) || Number.isNaN(gain)) continue;
      filters.push({ kind, fc, gain, q: Number.isNaN(q) ? 0.707 : q });
    }
  }
  if (!seen) throw err('invalidValue', 'no EQ found: expected Preamp / Filter / GraphicEQ lines (AutoEq / Equalizer APO)');
  const notes: string[] = [];
  const half = (v: number) => Math.round(v * 2) / 2 + 0;
  const fit = (label: string, v: number, lim: number) => {
    const r = half(v);
    const c = Math.max(-lim, Math.min(lim, r));
    if (c !== r) notes.push(`${label}: ${v.toFixed(1)} dB clamped to ${c.toFixed(1)} dB`);
    return c;
  };
  const graphicAt = (hz: number) => {
    if (!graphic.length) return 0;
    if (hz <= graphic[0][0]) return graphic[0][1];
    const last = graphic[graphic.length - 1];
    if (hz >= last[0]) return last[1];
    const i = graphic.findIndex((p) => p[0] > hz);
    const [f0, g0] = graphic[i - 1];
    const [f1, g1] = graphic[i];
    return g0 + ((g1 - g0) * (Math.log(hz) - Math.log(f0))) / (Math.log(f1) - Math.log(f0));
  };
  const bands = BAND_HZ.map((hz, i) =>
    fit(`Band ${hz} Hz`, filters.reduce((s, f) => s + biquadDb(f.kind, f.fc, f.gain, f.q, hz), 0) + graphicAt(hz), 12),
  );
  if (filters.length) notes.unshift('Filters were evaluated at the 10 band centres; an approximation.');
  return { bands, preamp: fit('Pre-amp', preamp, 6), notes };
}

export function createMockBackend(): Backend {
  const q = typeof location !== 'undefined' ? new URLSearchParams(location.search) : null;
  const mock = new MockBackend(q?.get('mock') === 'disconnected', q?.get('mock') === 'nomixer');
  (window as unknown as { __g6mock: MockBackend }).__g6mock = mock;
  return mock;
}
