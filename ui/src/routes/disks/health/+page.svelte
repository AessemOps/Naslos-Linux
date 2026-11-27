<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { formatBytes, fmtTime } from '$lib/format';
  import { parseJsonSafe } from '$lib/storage';
  import type { Tone } from '$lib/status';

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

  let data: DiskHealthResponse | null = null;
  let loading = true;
  let refreshing = false;
  let error = '';
  let lastUpdated = '';
  let timer: ReturnType<typeof setInterval> | undefined;

  async function load() {
    refreshing = true;
    error = '';
    try {
      const res = await fetch('/api/disks/health');
      data = await parseJsonSafe<DiskHealthResponse>(res, 'Loading disk health');
      lastUpdated = fmtTime(new Date());
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

  // One tone per disk from its SMART health, ZFS vdev state, and the warning
  // thresholds the API returned. Badges render the computed tone through the
  // shared StatusBadge so there is no per-page colour table.
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
      return 'neutral';
    }
    return 'ok';
  }

  function severityRank(t: Tone): number {
    switch (t) {
      case 'bad':
        return 0;
      case 'warn':
        return 1;
      case 'neutral':
        return 2;
      default:
        return 3;
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
  $: neutral = disks.filter((d) => tone(d) === 'neutral').length;
</script>

<div class="mx-auto max-w-7xl">
  <PageHeader
    title="Disk Health"
    subtitle="SMART health and ZFS vdev state for every whole disk. Read-only: sleeping disks are not woken."
    breadcrumbs={[{ label: 'Storage' }, { label: 'Disks', href: '/disks' }, { label: 'Health' }]}
  >
    <div slot="actions" class="flex flex-wrap items-center gap-3">
      {#if lastUpdated}<span class="text-xs text-gray-500">Updated {lastUpdated}</span>{/if}
      <Button variant="secondary" loading={refreshing} on:click={load}>Refresh</Button>
    </div>
  </PageHeader>

  {#if data && !data.smartctlAvailable}
    <div role="alert" class="callout callout-warn">
      <p class="font-medium">SMART is unavailable on this node.</p>
      <p class="mt-1 text-sm">
        {data.error || 'The agent could not read SMART data; ZFS state is still shown.'}
      </p>
    </div>
  {/if}

  <AsyncState
    {loading}
    error={error}
    empty={disks.length === 0}
    loadingText="Loading disk health…"
    emptyTitle="No whole disks were reported by the node."
    emptyDetail="Only whole disks are listed; partitions and virtual devices are not."
    emptyIcon="hard-drive"
    onRetry={load}
  >
    <div class="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-5">
      <div class="rounded-lg border border-naslos-border bg-naslos-surface p-4">
        <p class="text-xs text-gray-500">Total</p>
        <p class="text-2xl font-semibold">{disks.length}</p>
      </div>
      <div class="rounded-lg border border-naslos-border bg-naslos-surface p-4">
        <p class="mb-1 text-xs text-gray-500">Healthy</p>
        <StatusBadge tone="ok" label={String(healthy)} />
      </div>
      <div class="rounded-lg border border-naslos-border bg-naslos-surface p-4">
        <p class="mb-1 text-xs text-gray-500">Warning</p>
        <StatusBadge tone="warn" label={String(warning)} />
      </div>
      <div class="rounded-lg border border-naslos-border bg-naslos-surface p-4">
        <p class="mb-1 text-xs text-gray-500">Failed</p>
        <StatusBadge tone="bad" label={String(failed)} />
      </div>
      <div class="rounded-lg border border-naslos-border bg-naslos-surface p-4">
        <p class="mb-1 text-xs text-gray-500">Unknown</p>
        <StatusBadge tone="neutral" label={String(neutral)} />
      </div>
    </div>

    <DataTable>
      <table>
        <thead>
          <tr>
            <th scope="col">Disk</th>
            <th scope="col">Model / serial</th>
            <th scope="col">Pool / vdev</th>
            <th scope="col">ZFS</th>
            <th scope="col">SMART</th>
            <th scope="col" class="text-right">Temp</th>
            <th scope="col" class="text-right">Hours</th>
            <th scope="col" class="text-right">Wear</th>
            <th scope="col" class="text-right">SMART errors (R/P/U)</th>
            <th scope="col">Details</th>
          </tr>
        </thead>
        <tbody>
          {#each sortedDisks as disk}
            {@const t = tone(disk)}
            <tr class="align-top">
              <td class="whitespace-nowrap">
                <span class="font-mono font-medium">{disk.device}</span>
                {#if disk.isSystemDisk}
                  <span class="ml-2"><StatusBadge tone="neutral" label="system" /></span>
                {/if}
                <div class="text-xs text-gray-500">
                  {disk.type || '—'} · {formatBytes(disk.size)}
                </div>
              </td>
              <td>
                <div>{disk.model || 'Unknown model'}</div>
                <div class="font-mono text-xs text-gray-500">{disk.serial || 'no serial'}</div>
              </td>
              <td class="whitespace-nowrap">
                {#if disk.zfs}
                  <a class="text-naslos-link hover:underline" href={`/pools/${disk.zfs.pool}`}>
                    {disk.zfs.pool}
                  </a>
                  <div class="font-mono text-xs text-gray-500">{disk.zfs.vdev}</div>
                {:else if disk.inPool}
                  {disk.inPool}
                {:else}
                  <span class="text-gray-500">—</span>
                {/if}
              </td>
              <td>
                <StatusBadge tone={t} label={zfsLabel(disk)} />
                {#if disk.zfs}
                  <div class="mt-1 font-mono text-xs text-gray-500">
                    {disk.zfs.readErrors}/{disk.zfs.writeErrors}/{disk.zfs.cksumErrors}
                  </div>
                {/if}
              </td>
              <td>
                <StatusBadge tone={t} label={smartLabel(disk)} />
                {#if disk.smart.enabled === false && disk.smart.available}
                  <div class="mt-1 text-xs text-gray-500">SMART disabled</div>
                {/if}
              </td>
              <td class="text-right font-mono">
                {disk.smart.temperatureC !== undefined ? `${disk.smart.temperatureC}°C` : '—'}
              </td>
              <td class="text-right font-mono">{valueOrDash(disk.smart.powerOnHours)}</td>
              <td class="text-right font-mono">
                {disk.smart.wearPercent !== undefined ? `${disk.smart.wearPercent}%` : '—'}
              </td>
              <td class="whitespace-nowrap text-right font-mono">
                {valueOrDash(disk.smart.reallocatedSectors)} /
                {valueOrDash(disk.smart.pendingSectors)} /
                {valueOrDash(disk.smart.uncorrectableSectors)}
              </td>
              <td class="max-w-xs text-xs text-gray-400">
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
                  <span class="text-gray-500">—</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </DataTable>

    {#if neutral > 0}
      <p role="status" class="mt-3 text-xs text-gray-500">
        {neutral} disk{neutral === 1 ? '' : 's'} {neutral === 1 ? 'has' : 'have'} no usable SMART
        data (virtio, USB bridges and some RAID controllers do not expose it).
      </p>
    {/if}
    {#if failed > 0}
      <p role="alert" class="mt-3 text-xs text-red-300">
        A failed disk should be replaced and the pool resilvered. Do not run a destructive test on a
        disk that holds data.
      </p>
    {/if}
  </AsyncState>
</div>
