import { toneFor, type Tone } from './status';

/**
 * Tone for a Helm release status (not a generic job state). `toneFor` owns the
 * shared vocabulary; only the app-specific words that differ are handled here,
 * so app badges and the rest of the console cannot drift apart.
 */
export function statusTone(status: string): Tone {
  const s = (status || '').toLowerCase();
  // A catalog entry that disappeared is a warning, not a neutral unknown.
  if (s === 'missing') return 'warn';
  if (s.startsWith('pending') || s === 'uninstalling') return 'warn';
  return toneFor(status, 'neutral');
}
