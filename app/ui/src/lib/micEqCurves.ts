// Built-in mic EQ presets (the chips in the Mic tab) and the rules shared with the
// backend (app/src-tauri/src/mic_eq_curves.rs) for user-saved mic curve names.
// Bands are mic EQ bands 1..8 in dB; their frequencies are unknown, so no Hz labels.
// The presets come from the upstream linuxblaster_control capture; the real names are
// unknown. Only Preset 1 is known to equal what the G6 holds (see docs/protocol-g6.md).

export { MAX_CURVE_NAME } from './eqCurves';

export interface MicCurveValues {
  bands: number[];
}

export interface NamedMicCurve extends MicCurveValues {
  name: string;
}

const PRESET_BANDS: readonly (readonly number[])[] = [
  [-3, -4, 0, 2, 3, -3, 4, 5],
  [-3, -4, 0, 2, 4, -2, 2, 4],
  [-2, -3, 3, 4, 4, -4, 3, 2],
  [-3, -5, 0, 4, 0, -3, 0, 0],
  [-2, -3, 2, 4, 4, 0, -3, 2],
  [-5, -4, -2, 0, 3, 4, 6, 7],
  [0, 3, -2, -4, -4, -2, 5, 7],
  [0, 0, 2, 2, 3, -4, 2, 4],
  [0, 0, 2, 2, -2, 0, -4, 4],
  [0, 2, -2, 0, 3, 5, 6, 5],
  // Upstream calls this one DM_1.
  [0, 8, 0, 12, 12, 4, 8, 10],
];

export const MIC_BUILTIN_CURVES: readonly NamedMicCurve[] = PRESET_BANDS.map((bands, i) => ({
  name: `Preset ${i + 1}`,
  bands: [...bands],
}));

/** Name of the chip shown when no preset matches. */
export const MIC_CUSTOM_NAME = 'Custom';

/** Names a saved mic curve cannot take (compared case-insensitively). */
export const MIC_RESERVED_NAMES: readonly string[] = [...MIC_BUILTIN_CURVES.map((c) => c.name), MIC_CUSTOM_NAME];

/** Labels of the 8 mic EQ bands. */
export const MIC_BAND_LABELS = ['1', '2', '3', '4', '5', '6', '7', '8'];

/** Same 8 gains (multiples of 0.5; the tolerance only absorbs float noise). */
export function micMatches(a: MicCurveValues, b: MicCurveValues): boolean {
  return a.bands.length === b.bands.length && a.bands.every((v, i) => Math.abs(v - b.bands[i]) < 0.01);
}
