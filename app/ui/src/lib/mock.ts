// In-memory stand-in for the Rust backend, used automatically when the UI runs
// outside Tauri (`npm run dev` in a browser). It follows app/API.md: same
// commands, same errors, same events, same Scout / Direct Mode rules. It is
// not a simulation of the device.
//
// Dev handle in the browser console: `__g6mock.switchOutput()`,
// `.disconnect()`, `.connect()`, `.pressScout()`, `.pressSbx()`,
// `.failNext('deviceTimeout')`. `?mock=disconnected` starts disconnected.

import type {
  ApiError,
  AppState,
  Backend,
  DacFilter,
  DolbyMode,
  EffectName,
  Effects,
  ErrorCode,
  Eq,
  EqCurve,
  Mic,
  MicBoost,
  Output,
  SmartVolumeMode,
} from './api';
import { MAX_CURVE_NAME, RESERVED_NAMES } from './eqCurves';

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
    mic: { voiceClarity: true, noiseReduction: 0, eqEnabled: micEq, aec: false, smartVolume: false },
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
  private micBoost: MicBoost = 30;
  private dacFilter: DacFilter = 'fast-min';
  private dolby: DolbyMode = 'normal';
  private presets = new Map<string, PresetEntry>();
  private eqCurves: EqCurve[] = [];
  private activePreset: string | null = 'Default';
  private launch = true;
  private failCode: ErrorCode | null = null;

  constructor(startDisconnected: boolean) {
    if (startDisconnected) this.connection = 'disconnected';
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
    if (cmd === 'get_state' || cmd === 'hide_panel' || cmd === 'set_app_theme' || cmd === 'list_presets' || cmd === 'list_eq_curves') {
      return this.read(cmd);
    }
    if (this.failCode) {
      const code = this.failCode;
      this.failCode = null;
      throw err(code, `Mock failure: ${code}`);
    }
    if (cmd === 'refresh_state') return this.snapshot();
    if (this.connection === 'disconnected' && cmd !== 'delete_preset' &&
      cmd !== 'rename_preset' &&
      cmd !== 'delete_eq_curve' &&
      cmd !== 'set_launch_at_login') {
      throw err('notConnected', 'The G6 is not connected.');
    }
    this.write(cmd, args);
    const s = this.snapshot();
    this.emit('state-changed', s);
    return s;
  }

  // --- Dev handle --------------------------------------------------------

  switchOutput(): void {
    this.output = this.output === 'headphones' ? 'speakers' : 'headphones';
    this.emit('output-changed', { output: this.output });
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
    if (cmd === 'list_presets') return [...this.presets.keys()].sort();
    if (cmd === 'list_eq_curves') return clone(this.eqCurves);
    return null;
  }

  private snapshot(): AppState {
    const p = clone(this.profiles[this.output]);
    let sbx = this.sbx;
    let scout = this.scout;
    const eq = p.eq;
    // Scout owns the EQ curve and Smart Volume; SBX is off while it is on.
    if (scout) {
      eq.enabled = true;
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
      presets: [...this.presets.keys()].sort(),
      activePreset: this.activePreset,
      eqCurves: clone(this.eqCurves),
      launchAtLogin: this.launch,
    };
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
    this.scout = on;
    if (on) this.sbx = false;
  }

  private num(v: unknown, lo: number, hi: number, step: number): number {
    const n = Number(v);
    const k = (n - lo) / step;
    if (!Number.isFinite(n) || n < lo || n > hi || Math.abs(k - Math.round(k)) > 1e-6) {
      throw err('invalidValue', `Value ${String(v)} is out of range.`);
    }
    return n;
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
        const band = this.num(a.band, 0, 9, 1);
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
        // Not Scout-owned: only Direct Mode blocks it.
        this.guardPlayback();
        p.eq.preamp = this.num(a.db, -6, 6, 0.5);
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
        this.micBoost = this.num(a.db, 0, 30, 10) as MicBoost;
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
      case 'set_mic_aec':
        p.mic.aec = Boolean(a.enabled);
        break;
      case 'set_mic_smart_volume':
        p.mic.smartVolume = Boolean(a.enabled);
        break;
      case 'set_dac_filter':
        this.dacFilter = a.filter as DacFilter;
        break;
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
        break;
      }
      case 'delete_preset': {
        const name = String(a.name ?? '');
        if (!this.presets.delete(name)) throw err('presetNotFound', `No profile named "${name}".`);
        if (this.activePreset === name) this.activePreset = null;
        break;
      }
      case 'set_launch_at_login':
        this.launch = Boolean(a.enabled);
        break;
      default:
        throw err('notImplemented', `Unknown command ${cmd}.`);
    }
  }
}

export function createMockBackend(): Backend {
  const q = typeof location !== 'undefined' ? new URLSearchParams(location.search) : null;
  const mock = new MockBackend(q?.get('mock') === 'disconnected');
  (window as unknown as { __g6mock: MockBackend }).__g6mock = mock;
  return mock;
}
