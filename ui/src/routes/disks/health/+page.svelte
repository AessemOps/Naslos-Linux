<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  interface SmartInfo {
    available: boolean;
    enabled: boolean;
    health: string;
    temperatureC?: number;
    powerOnHours?: number;
    wearPercent?: number;
    reallocatedSectors?: number;
    pendingSectors?: number;
    uncorrectableSectors?: number;
    mediaErrors?: number;
    errorCount?: number;
    standby: boolean;
    message: string;
  }

  interface ZfsDiskHealth {
    pool: string;
    vdev: string;
    state: string;
    readErrors: number;
    writeErrors: number;
    cksumErrors: number;
  }

  interface DiskHealthEntry {
    device: string;
    size: number;
    isSystemDisk: boolean;
    model: string;
    serial: string;
    busPath: string;
    type: string;
    signatures: string[];
    inPool?: string;
    smart: SmartInfo;
    zfs?: ZfsDiskHealth;
  }

  interface DiskHealthResponse {
    smartctlAvailable: boolean;
    error: string;
    temperatureWarningC: number;
    wearWarningPercent: number;
    disks: DiskHealthEntry[];
  }

  type Tone = 'ok' | 'warn' | 'bad' | 'unknown';

  let data: DiskHealthResponse | null = null;
  let loading = true;
  let refreshing = false;
  let error = '';
  let lastUpdated = '';
  let timer: ReturnType<typeof setInterval> | undefined;

  async function parseJsonSafe(res: Response, what: string) {
    const text = await res.text();
    if (!res.ok) {
      throw new Error(`${what} failed: HTTP ${res.status} — ${text.slice(0, 200)}`);
    }
    const contentType = res.headers.get('content-type') || '';
    if (!contentType.includes('application/json')) {
      throw new Error(
        `${what} failed: expected JSON but got "${contentType || 'unknown content type'}" — ${text.slice(0, 200)}`
      );
    }
    try {
      return JSON.parse(text);
    } catch {
      throw new Error(`${what} failed: invalid JSON response — ${text.slice(0, 200)}`);
    }
  }

  async function load() {
    refreshing = true;
    error = '';
    try {
      const res = await fetch('/api/disks/health');
      data = await parseJsonSafe(res, 'Loading disk health');
      lastUpdated = new Date().toLocaleTimeString();
    } catch (e: any) {
      error = e?.message || String(e);
    } finally {
      loading = false;
      refreshing = false;
    }
  }

  onMount(() => {
    load();
    timer = setInterval(load, 60_000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  function stateUpper(disk: DiskHealthEntry): string {
    return (disk.zfs?.state || '').toUpperCase();
  }

  function tone(disk: DiskHealthEntry): Tone {
    const health = (disk.smart.health || '').toUpperCase();
    const zfs = stateUpper(disk);
    if (
      health === 'FAILED' ||
      zfs.includes('FAULTED') ||
      zfs.includes('OFFLINE') ||
      zfs.includes('UNAVAIL') ||
      zfs.includes('REMOVED')
    ) {
      return 'bad';
    }
    if (
      zfs.includes('DEGRADED') ||
      (disk.smart.temperatureC !== undefined &&
        disk.smart.temperatureC > (data?.temperatureWarningC ?? 55)) ||
      (disk.smart.wearPercent !== undefined &&
        disk.smart.wearPercent >= (data?.wearWarningPercent ?? 80))
    ) {
      return 'warn';
    }
    if (!disk.smart.available || health === 'UNKNOWN' || health === '') {
      return 'unknown';
    }
    return 'ok';
  }

  function severityRank(t: Tone): number {
    switch (t) {
      case 'bad':
        return 0;
      case 'warn':
        return 1;
      case 'unknown':
        return 2;
      default:
        return 3;
    }
  }

  function toneBadge(t: Tone): string {
    switch (t) {
      case 'bad':
        return 'bg-red-900/50 text-red-300';
      case 'warn':
        return 'bg-amber-900/50 text-amber-300';
      case 'ok':
        return 'bg-green-900/50 text-green-400';
      default:
        return 'bg-naslos-border text-gray-400';
    }
  }

  function smartLabel(disk: DiskHealthEntry): string {
    if (!disk.smart.available) {
      if (disk.smart.standby) return 'standby';
      return 'unavailable';
    }
    return disk.smart.health || 'unknown';
  }

  function zfsLabel(disk: DiskHealthEntry): string {
    return disk.zfs?.state || '—';
  }

  function formatSize(bytes: number): string {
    if (!bytes) return '—';
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit++;
    }
    return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
  }

  function valueOrDash(value: number | undefined): string {
    return value === undefined ? '—' : String(value);
  }

  $: disks = data?.disks ?? [];
  $: sortedDisks = [...disks].sort(
    (a, b) => severityRank(tone(a)) - severityRank(tone(b)) || a.device.localeCompare(b.device)
  );
  $: healthy = disks.filter((d) => tone(d) === 'ok').length;
  $: warning = disks.filter((d) => tone(d) === 'warn').length;
  $: failed = disks.filter((d) => tone(d) === 'bad').length;
  $: unknown = disks.filter((d) => tone(d) === 'unknown').length;
</script>

<div class="max-w-7xl mx-auto">
  <div class="flex flex-wrap items-start justify-between gap-4 mb-6">
    <div>
      <h1 class="text-3xl font-bold mb-2">Disk Health</h1>
      <p class="text-gray-400">
        SMART health and ZFS vdev state for every whole disk. Read-only: sleeping disks are not woken.
      </p>
    </div>
    <div class="flex items-center gap-3">
      {#if lastUpdated}<span class="text-xs text-gray-500">Updated {lastUpdated}</span>{/if}
      <button
        type="button"
        class="px-4 py-2 rounded-lg bg-naslos-primary hover:bg-naslos-accent text-white text-sm disabled:opacity-50"
        on:click={load}
        disabled={refreshing}
      >
        {refreshing ? 'Refreshing…' : 'Refresh'}
      </button>
    </div>
  </div>

  {#if error}
    <div class="mb-6 p-4 rounded-lg border border-red-800 bg-red-950/40 text-red-300">
      {error}
    </div>
  {/if}

  {#if data && !data.smartctlAvailable}
    <div class="mb-6 p-4 rounded-lg border border-amber-800 bg-amber-950/30 text-amber-200">
      <p class="font-medium">SMART is unavailable on this node.</p>
      <p class="text-sm text-amber-300/80 mt-1">
        {data.error || 'The agent could not read SMART data; ZFS state is still shown.'}
      </p>
    </div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading disk health…</p>
  {:else if disks.length === 0 && !error}
    <div class="p-8 rounded-lg border border-naslos-border bg-naslos-surface text-center text-gray-400">
      No whole disks were reported by the node.
    </div>
  {:else}
    <div class="grid grid-cols-2 sm:grid-cols-5 gap-3 mb-6">
      <div class="p-4 rounded-lg bg-naslos-surface border border-naslos-border">
        <p class="text-xs text-gray-500">Total</p>
        <p class="text-2xl font-semibold">{disks.length}</p>
      </div>
      <div class="p-4 rounded-lg bg-naslos-surface border border-green-900/60">
        <p class="text-xs text-gray-500">Healthy</p>
        <p class="text-2xl font-semibold text-green-400">{healthy}</p>
      </div>
      <div class="p-4 rounded-lg bg-naslos-surface border border-amber-900/60">
        <p class="text-xs text-gray-500">Warning</p>
        <p class="text-2xl font-semibold text-amber-300">{warning}</p>
      </div>
      <div class="p-4 rounded-lg bg-naslos-surface border border-red-900/60">
        <p class="text-xs text-gray-500">Failed</p>
        <p class="text-2xl font-semibold text-red-400">{failed}</p>
      </div>
      <div class="p-4 rounded-lg bg-naslos-surface border border-naslos-border">
        <p class="text-xs text-gray-500">Unknown</p>
        <p class="text-2xl font-semibold text-gray-400">{unknown}</p>
      </div>
    </div>

    <div class="overflow-x-auto rounded-lg border border-naslos-border">
      <table class="min-w-full text-sm">
        <thead class="bg-naslos-surface text-gray-400">
          <tr>
            <th class="text-left font-medium px-3 py-2">Disk</th>
            <th class="text-left font-medium px-3 py-2">Model / serial</th>
            <th class="text-left font-medium px-3 py-2">Pool / vdev</th>
            <th class="text-left font-medium px-3 py-2">ZFS</th>
            <th class="text-left font-medium px-3 py-2">SMART</th>
            <th class="text-right font-medium px-3 py-2">Temp</th>
            <th class="text-right font-medium px-3 py-2">Hours</th>
            <th class="text-right font-medium px-3 py-2">Wear</th>
            <th class="text-right font-medium px-3 py-2">SMART errors (R/P/U)</th>
            <th class="text-left font-medium px-3 py-2">Details</th>
          </tr>
        </thead>
        <tbody>
          {#each sortedDisks as disk}
            {@const t = tone(disk)}
            <tr class="border-t border-naslos-border/70 align-top">
              <td class="px-3 py-3 whitespace-nowrap">
                <span class="font-medium">{disk.device}</span>
                {#if disk.isSystemDisk}
                  <span class="ml-2 text-xs px-1.5 py-0.5 rounded bg-naslos-border text-gray-300">system</span>
                {/if}
                <div class="text-xs text-gray-500">{disk.type || '—'} · {formatSize(disk.size)}</div>
              </td>
              <td class="px-3 py-3">
                <div>{disk.model || 'Unknown model'}</div>
                <div class="text-xs text-gray-500">{disk.serial || 'no serial'}</div>
              </td>
              <td class="px-3 py-3 whitespace-nowrap">
                {#if disk.zfs}
                  <a class="text-naslos-primary hover:underline" href={`/pools/${disk.zfs.pool}`}>
                    {disk.zfs.pool}
                  </a>
                  <div class="text-xs text-gray-500">{disk.zfs.vdev}</div>
                {:else if disk.inPool}
                  {disk.inPool}
                {:else}
                  <span class="text-gray-500">—</span>
                {/if}
              </td>
              <td class="px-3 py-3">
                <span class={`inline-block px-2 py-0.5 rounded text-xs ${toneBadge(t)}`}>
                  {zfsLabel(disk)}
                </span>
                {#if disk.zfs}
                  <div class="text-xs text-gray-500 mt-1">
                    {disk.zfs.readErrors}/{disk.zfs.writeErrors}/{disk.zfs.cksumErrors}
                  </div>
                {/if}
              </td>
              <td class="px-3 py-3">
                <span class={`inline-block px-2 py-0.5 rounded text-xs ${toneBadge(t)}`}>
                  {smartLabel(disk)}
                </span>
                {#if disk.smart.enabled === false && disk.smart.available}
                  <div class="text-xs text-gray-500 mt-1">SMART disabled</div>
                {/if}
              </td>
              <td class="px-3 py-3 text-right">
                {disk.smart.temperatureC !== undefined ? `${disk.smart.temperatureC}°C` : '—'}
              </td>
              <td class="px-3 py-3 text-right">{valueOrDash(disk.smart.powerOnHours)}</td>
              <td class="px-3 py-3 text-right">
                {disk.smart.wearPercent !== undefined ? `${disk.smart.wearPercent}%` : '—'}
              </td>
              <td class="px-3 py-3 text-right whitespace-nowrap">
                {valueOrDash(disk.smart.reallocatedSectors)} /
                {valueOrDash(disk.smart.pendingSectors)} /
                {valueOrDash(disk.smart.uncorrectableSectors)}
              </td>
              <td class="px-3 py-3 text-xs text-gray-400 max-w-xs">
                {#if disk.smart.message}
                  <p>{disk.smart.message}</p>
                {/if}
                {#if disk.smart.mediaErrors !== undefined}
                  <p>Media errors: {disk.smart.mediaErrors}</p>
                {/if}
                {#if disk.smart.errorCount !== undefined}
                  <p>Error log: {disk.smart.errorCount}</p>
                {/if}
                {#if !disk.smart.message && disk.smart.mediaErrors === undefined && disk.smart.errorCount === undefined}
                  <span class="text-gray-600">—</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if unknown > 0}
      <p class="text-xs text-gray-500 mt-3">
        {unknown} disk{unknown === 1 ? '' : 's'} have no usable SMART data (virtio, USB bridges and
        some RAID controllers do not expose it).
      </p>
    {/if}
    {#if failed > 0}
      <p class="text-xs text-red-400 mt-3">
        A failed disk should be replaced and the pool resilvered. Do not run a destructive test on a
        disk that holds data.
      </p>
    {/if}
  {/if}
</div>
