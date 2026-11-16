import type { Output } from './api';

export const BAND_LABELS = ['31', '62', '125', '250', '500', '1k', '2k', '4k', '8k', '16k'];

/** "+1.5", "-2", "0" */
export function db(v: number): string {
  return (v > 0 ? '+' : '') + (Number.isInteger(v) ? String(v) : v.toFixed(1));
}

export function pct(v: number, lo: number, hi: number): string {
  return (((v - lo) / (hi - lo)) * 100).toFixed(1) + '%';
}

/** Label of the edited profile; neutral until the first output read. */
export function outputName(o: Output | null): string {
  return o === 'headphones' ? 'Headphones' : o === 'speakers' ? 'Speakers' : 'Output';
}
