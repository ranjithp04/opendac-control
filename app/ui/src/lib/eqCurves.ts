// Built-in EQ curves (the chips above the EQ curve) and the rules shared with
// the backend (app/src-tauri/src/eq_curves.rs) for user-saved curve names.
// Bands are 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz in dB; pre-amp in dB.

export interface CurveValues {
  bands: number[];
  preamp: number;
}

export interface NamedCurve extends CurveValues {
  name: string;
}

// Values from reports/G6 ten band EQ presets.md: largest slider <= +5 dB, summed peak <= ~+6 dB,
// pre-amp = -(modelled peak) rounded up to 0.5 dB (0 for cut-only curves).
export const BUILTIN_CURVES: readonly NamedCurve[] = [
  { name: 'Flat', bands: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0], preamp: 0 },
  { name: 'Bass boost', bands: [5, 4, 1.5, 0, 0, 0, 0, 0, 0, 0], preamp: -6 },
  { name: 'Bass cut', bands: [-6, -4, -2, 0, 0, 0, 0, 0, 0, 0], preamp: 0 },
  { name: 'Treble boost', bands: [0, 0, 0, 0, 0, 0.5, 1, 2, 3, 4], preamp: -4.5 },
  { name: 'Warm', bands: [2, 1.5, 1, 0.5, 0, 0, -1, -2, -2.5, -3], preamp: -3 },
  { name: 'V-shape', bands: [4, 3.5, 1.5, 0, -1.5, -1.5, 0, 1.5, 3, 3.5], preamp: -5.5 },
  { name: 'Loudness', bands: [4.5, 3.5, 2.5, 1.5, 0.5, 0, 0, 0, 0, 2], preamp: -6 },
  { name: 'Speech', bands: [-6, -4, -2, -1.5, 0, 1, 2.5, 2, 0, -1], preamp: -3.5 },
  { name: 'Footsteps', bands: [-3, -2.5, -1, -1, -0.5, 0, 2, 3.5, 2, 0], preamp: -5 },
  { name: 'Small speakers', bands: [-6, -2, 1.5, 1.5, 0, 0, 0.5, 1, 0, 0], preamp: -2 },
];

/** Name of the chip shown when no curve matches. */
export const CUSTOM_NAME = 'Custom';

/** Longest name of a saved curve, in characters. */
export const MAX_CURVE_NAME = 32;

/** Names a saved curve cannot take (compared case-insensitively). */
export const RESERVED_NAMES: readonly string[] = [...BUILTIN_CURVES.map((c) => c.name), CUSTOM_NAME];

/** Same bands and same pre-amp (gains are multiples of 0.5; the tolerance only absorbs float noise). */
export function matches(a: CurveValues, b: CurveValues): boolean {
  return (
    Math.abs(a.preamp - b.preamp) < 0.01 &&
    a.bands.length === b.bands.length &&
    a.bands.every((v, i) => Math.abs(v - b.bands[i]) < 0.01)
  );
}
