<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import AsyncState from './AsyncState.svelte';
  import Skeleton from './Skeleton.svelte';
  import CpuDetails from './dashboard/CpuDetails.svelte';
  import HistoryCard from './dashboard/HistoryCard.svelte';
  import PoolsCard from './dashboard/PoolsCard.svelte';
  import StatCard from './dashboard/StatCard.svelte';
  import SystemInfoCard from './dashboard/SystemInfoCard.svelte';
  import type { DashboardData } from './dashboard/types';
  import { api } from '$lib/api';
  import { formatBytes } from '$lib/format';

  // Auto-refresh: poll the dashboard API every 5 seconds. A request that is
  // still in flight when the next tick fires is skipped (inFlight guard). The
  // intervals are created onMount so nothing runs during SSR, and cleared on
  // unmount.
  const REFRESH_INTERVAL_MS = 5000;
  const SAMPLE_LIMIT = 24;

  let data: DashboardData = {};
  let loading = true;
  let error = '';
  let inFlight = false;
  let destroyed = false;
  let refreshTimer: ReturnType<typeof setInterval> | null = null;

  // The stat cards own a short live sparkline built from the poll samples. No
  // extra requests: each successful dashboard read appends one point.
  let cpuSamples: number[] = [];
  let memorySamples: number[] = [];
  let storageSamples: number[] = [];
  let uptimeSamples: number[] = [];

  function append(samples: number[], value: number | undefined | null): number[] {
    if (value == null || !Number.isFinite(value)) return samples;
    return [...samples, value].slice(-SAMPLE_LIMIT);
  }

  async function loadDashboard() {
    if (inFlight || destroyed) return;
    inFlight = true;
    try {
      const next = await api<DashboardData>('/api/dashboard');
      if (destroyed) return;
      data = next;
      error = '';
      cpuSamples = append(cpuSamples, next.cpu?.usage);
      memorySamples = append(memorySamples, next.memory?.usage);
      storageSamples = append(storageSamples, next.disk?.usage);
      uptimeSamples = append(uptimeSamples, next.system?.uptime);
    } catch (e) {
      if (!destroyed) {
        error = 'Failed to load dashboard data: ' + (e instanceof Error ? e.message : String(e));
      }
    } finally {
      inFlight = false;
      if (!destroyed) loading = false;
    }
  }

  function retry() {
    loading = true;
    error = '';
    loadDashboard();
  }

  onMount(() => {
    loadDashboard();
    refreshTimer = setInterval(loadDashboard, REFRESH_INTERVAL_MS);
  });

  onDestroy(() => {
    destroyed = true;
    if (refreshTimer) clearInterval(refreshTimer);
  });

  // ZFS ARC is reclaimable cache, so show it beside the raw used figure instead
  // of letting it look like anonymous application memory.
  $: memoryArc = data.memory?.arcSize ?? 0;
  $: memoryArcPercent =
    data.memory && data.memory.total > 0 ? (memoryArc / data.memory.total) * 100 : 0;
  $: memoryNonArc = Math.max(0, (data.memory?.used ?? 0) - memoryArc);

  // A real 0% is a value, not a missing reading: only null/undefined render as
  // an em dash. The `||` this replaces hid 0 and every sub-0.05% figure.
  $: cpuValue = data.cpu?.usage == null ? '—' : data.cpu.usage.toFixed(1);
  $: memoryValue = data.memory?.usage == null ? '—' : data.memory.usage.toFixed(1);
  $: storageValue = data.disk?.usage == null ? '—' : data.disk.usage.toFixed(1);

  // Thresholds are the same green/amber/red bands the core bars use.
  function usageTone(value: number | undefined): 'ok' | 'warn' | 'bad' | 'neutral' {
    if (value == null) return 'neutral';
    if (value >= 90) return 'bad';
    if (value >= 75) return 'warn';
    return 'ok';
  }

  function formatUptime(seconds: number): string {
    if (!seconds) return '—';
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const mins = Math.floor((seconds % 3600) / 60);
    if (days > 0) return `${days}d ${hours}h ${mins}m`;
    if (hours > 0) return `${hours}h ${mins}m`;
    return `${mins}m`;
  }

  function formatTemperature(value?: number | null): string {
    return value === undefined || value === null || Number.isNaN(value)
      ? '—'
      : `${value.toFixed(1)} °C`;
  }
</script>

{#if loading}
  <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4" aria-hidden="true">
    {#each [0, 1, 2, 3] as slot (slot)}
      <div class="card">
        <Skeleton height="h-3" width="w-20" />
        <div class="mt-3"><Skeleton height="h-8" width="w-28" /></div>
        <div class="mt-2"><Skeleton height="h-3" width="w-32" /></div>
        <div class="mt-3"><Skeleton height="h-9" /></div>
      </div>
    {/each}
  </div>
  <span class="sr-only" role="status" aria-busy="true" data-testid="page-loading">Loading dashboard...</span>
{:else if error}
  <AsyncState {error} onRetry={retry} />
{:else}
  <div class="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
    <StatCard
      label="CPU Usage"
      value={cpuValue}
      unit={cpuValue === '—' ? '' : '%'}
      tone={usageTone(data.cpu?.usage)}
      detail={`${data.cpu?.cores || 0} cores · ${formatTemperature(data.cpu?.temperatureC)}`}
      values={cpuSamples}
    />
    <StatCard
      label="Memory"
      value={memoryValue}
      unit={memoryValue === '—' ? '' : '%'}
      tone={usageTone(data.memory?.usage)}
      detail={`${formatBytes(data.memory?.used || 0)} / ${formatBytes(data.memory?.total || 0)}`}
      values={memorySamples}
    >
      {#if memoryArc > 0}
        <div class="mt-1 text-xs text-gray-500" data-testid="memory-arc">
          ZFS ARC {formatBytes(memoryArc)} ({memoryArcPercent.toFixed(1)}%)
        </div>
        <div class="text-xs text-gray-500">
          Apps &amp; system {formatBytes(memoryNonArc)}{data.memory?.arcMax ? ` · ARC max ${formatBytes(data.memory.arcMax)}` : ''}
        </div>
      {/if}
    </StatCard>
    <StatCard
      label="Storage"
      value={storageValue}
      unit={storageValue === '—' ? '' : '%'}
      tone={usageTone(data.disk?.usage)}
      detail={`${formatBytes(data.disk?.used || 0)} / ${formatBytes(data.disk?.total || 0)}`}
      values={storageSamples}
    />
    <StatCard
      label="Uptime"
      value={formatUptime(data.system?.uptime || 0)}
      tone="neutral"
      detail={data.system?.hostname || '—'}
      values={uptimeSamples}
    />
  </div>

  <div class="mt-6 space-y-6">
    <HistoryCard />
    <CpuDetails cpu={data.cpu} />
    <PoolsCard pools={data.zfs?.pools ?? []} onDeleted={loadDashboard} />
    <SystemInfoCard
      system={data.system}
      network={data.network}
      updatedAt={data.updatedAt ?? ''}
    />
  </div>
{/if}
