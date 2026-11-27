// Shared usage bands. The KPI cards and the per-core CPU bars must agree on
// when a value turns amber/red, so both read these thresholds.
export const USAGE_WARN_PERCENT = 75;
export const USAGE_BAD_PERCENT = 90;

export function usageTone(value: number | null | undefined): 'ok' | 'warn' | 'bad' | 'neutral' {
  if (value == null) return 'neutral';
  if (value >= USAGE_BAD_PERCENT) return 'bad';
  if (value >= USAGE_WARN_PERCENT) return 'warn';
  return 'ok';
}
