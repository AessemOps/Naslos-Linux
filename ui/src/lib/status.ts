// Single state -> badge tone mapping shared by every status surface. The
// component `StatusBadge.svelte` is the only renderer; this module is exported
// so pages that need the tone for their own markup (charts, dots) use the same
// table instead of copying the word lists.
export type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

/**
 * Map a state word to a badge tone.
 *
 * `fallback` is returned for states the table does not know. Callers that must
 * fail closed (pool health) pass `'bad'`; everything else gets `'neutral'`.
 */
export function toneFor(value: string, fallback: Tone = 'neutral'): Tone {
  const s = (value || '').toLowerCase();
  if (
    s === 'online' ||
    s === 'running' ||
    s === 'deployed' ||
    s === 'succeeded' ||
    s === 'ok' ||
    s === 'ready' ||
    s === 'active' ||
    s === 'passed'
  ) {
    return 'ok';
  }
  if (
    s === 'degraded' ||
    s === 'pending' ||
    s === 'uninstalling' ||
    s === 'standby' ||
    s === 'unknown' ||
    s === 'warn' ||
    s === 'warning' ||
    s === 'never run'
  ) {
    return 'warn';
  }
  if (
    s === 'faulted' ||
    s === 'failed' ||
    s === 'suspended' ||
    s === 'removed' ||
    s === 'unavail' ||
    s === 'error' ||
    s === 'not-ready' ||
    s === 'disabled'
  ) {
    // 'offline' is intentionally NOT bad: an administratively offlined vdev
    // is neutral in the device tree; pool-level surfaces pass fallback='bad'
    // so an OFFLINE pool still reads as an alert.
    return 'bad';
  }
  if (s === 'loading' || s === 'provisioning') return 'info';
  return fallback;
}
