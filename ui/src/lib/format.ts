import type { AppUsage } from './appTypes';

const BYTE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
const RATE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

function scaled(value: number | null | undefined, units: string[], base = 1024): string | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  let size = value;
  let unit = 0;
  while (size >= base && unit < units.length - 1) {
    size /= base;
    unit += 1;
  }
  const rounded = unit === 0 || size >= 10 ? Math.round(size).toString() : size.toFixed(1);
  return `${rounded} ${units[unit]}`;
}

/** Human byte size (binary units). `null`/`undefined` renders as an em dash. */
export function formatBytes(value: number | null | undefined): string {
  return scaled(value, BYTE_UNITS) ?? '—';
}

/** Human throughput (decimal units, as the plan's row copy uses MB/s). */
export function formatBytesPerSec(value: number | null | undefined): string {
  const text = scaled(value, RATE_UNITS, 1000);
  return text == null ? '—' : `${text}/s`;
}

/** Milli-cores as the compact `42m` form used by the Installed rows. */
export function formatMillicores(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  return `${Math.round(value)}m`;
}

/** The per-app aggregate line: `CPU 42m · RAM 512 MiB · ↓ 2.1 MB/s ↑ 40 KB/s`. */
export function formatUsage(usage: AppUsage | undefined): string {
  if (!usage) return '—';
  return [
    `CPU ${formatMillicores(usage.cpuMillicores)}`,
    `RAM ${formatBytes(usage.memoryBytes)}`,
    `↓ ${formatBytesPerSec(usage.networkRxBytesPerSec)}`,
    `↑ ${formatBytesPerSec(usage.networkTxBytesPerSec)}`
  ].join(' · ');
}
