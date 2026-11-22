<script lang="ts">
  import { onDestroy } from 'svelte';
  import LoadingState from './LoadingState.svelte';
  import Modal from './Modal.svelte';
  import Sparkline from './Sparkline.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { formatBytes, formatBytesPerSec } from '$lib/format';

  interface ReportSeriesMeta {
    key: string;
    kind: string;
    unit: string;
  }

  interface ReportMetadata {
    enabled: boolean;
    available: boolean;
    error?: string | null;
    ranges: Array<{ range: string; resolution: number; window_seconds: number; max_points: number }>;
    series: ReportSeriesMeta[];
  }

  interface ReportSeries {
    key: string;
    kind: string;
    unit: string;
    points: number[][];
  }

  interface ReportResponse {
    available: boolean;
    error?: string | null;
    resolution: number;
    sample_interval_seconds: number;
    series: ReportSeries[];
    ignored: string[];
  }

  interface DashboardData {
    cpu?: {
      usage: number;
      cores: number;
      model?: string;
      temperatureC?: number | null;
      loadAvg1?: number;
      loadAvg5?: number;
      loadAvg15?: number;
      perCore?: Array<{ core: number; usagePercent: number; frequencyMhz?: number }>;
    };
    memory?: { usage: number; total: number; used: number; available: number };
    disk?: { usage: number; total: number; used: number; free: number };
    zfs?: { poolCount: number; pools: any[] };
    system?: { hostname: string; uptime: number; os: string };
    network?: {
      bytesSent?: number;
      bytesRecv?: number;
      interfaces?: Array<{ name: string; ipAddress?: string }>;
    };
    updatedAt?: string;
  }

  // Interfaces with an address are the ones an operator can use; a node also has
  // dozens of veth/pseudo interfaces with none.
  $: addressedInterfaces = (data.network?.interfaces ?? []).filter((i) => i.ipAddress);

  let data: DashboardData = {};
  let loading = true;
  let error = '';

  // Auto-refresh: poll the dashboard API every 5 seconds. A request that is
  // still in flight when the next tick fires is skipped (inFlight guard).
  const REFRESH_INTERVAL_MS = 5000;
  let inFlight = false;
  let refreshTimer: ReturnType<typeof setInterval>;

  // Delete pool state
  let deleteTarget: string | null = null;
  let deleting = false;
  let deleteError = '';
  let deleteConfirmText = '';

  async function loadDashboard() {
    if (inFlight) return;
    inFlight = true;
    try {
      const res = await fetch('/api/dashboard');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      data = await res.json();
      error = '';
    } catch (e) {
      error = 'Failed to load dashboard data: ' + e;
    } finally {
      inFlight = false;
      loading = false;
    }
  }

  function confirmDelete(name: string) {
    deleteTarget = name;
    deleteError = '';
    deleteConfirmText = '';
  }

  function cancelDelete() {
    deleteTarget = null;
    deleteError = '';
    deleteConfirmText = '';
  }

  async function deletePool() {
    if (!deleteTarget) return;
    deleting = true;
    deleteError = '';
    try {
      const res = await fetch(
        `/api/volumes/zfs/${encodeURIComponent(deleteTarget)}?confirm=${encodeURIComponent(deleteTarget)}`,
        { method: 'DELETE' }
      );
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`HTTP ${res.status} — ${text.slice(0, 200)}`);
      }
      // Refresh dashboard to reflect the deletion
      await loadDashboard();
      cancelDelete();
    } catch (e) {
      deleteError = 'Failed to delete pool: ' + e;
    } finally {
      deleting = false;
    }
  }

  function formatTemperature(value?: number | null): string {
    return value === undefined || value === null || Number.isNaN(value)
      ? '—'
      : `${value.toFixed(1)} °C`;
  }

  function formatLoad(cpu?: DashboardData['cpu']): string {
    if (!cpu) return '—';
    return `${(cpu.loadAvg1 ?? 0).toFixed(2)} / ${(cpu.loadAvg5 ?? 0).toFixed(2)} / ${(cpu.loadAvg15 ?? 0).toFixed(2)}`;
  }

  function coreBarClass(usage: number): string {
    if (usage >= 90) return 'bg-red-500';
    if (usage >= 70) return 'bg-amber-400';
    return 'bg-naslos-primary';
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

  // formatUpdatedAt renders the ISO timestamp in the browser's locale and
  // hides the zero-value Go time (0001-01-01...) shown before first collection.
  function formatUpdatedAt(iso?: string): string {
    if (!iso || iso.startsWith('0001-01-01')) return '—';
    const d = new Date(iso);
    if (isNaN(d.getTime())) return '—';
    return d.toLocaleString();
  }

  // ---- Historical reporting (FR-RPT) -------------------------------------
  // Admin-only (the API gates /api/reports on the owner router). A non-admin
  // gets 403 and the section simply stays hidden instead of showing an error
  // on the dashboard every load.
  const HISTORY_REFRESH_MS = 60000;
  const historyRanges = [
    { value: '24h', label: '24 h' },
    { value: '7d', label: '7 d' },
    { value: '30d', label: '30 d' }
  ];
  let historyVisible = false;
  let historyMeta: ReportMetadata | null = null;
  let historySeries: ReportSeriesMeta[] = [];
  let historyRange = '24h';
  let historyMetric = '';
  let historyPoints: number[] = [];
  let historyMin = 0;
  let historyMax = 0;
  let historyInterval = 0;
  let historyUnit = 'percent';
  let historyLoading = false;
  let historyError = '';
  let historyTimer: ReturnType<typeof setInterval>;

  $: historyGroups = ['cpu', 'memory', 'network', 'zfs', 'disk']
    .map((kind) => ({
      kind,
      label: kind === 'zfs' ? 'ZFS' : kind.charAt(0).toUpperCase() + kind.slice(1),
      series: historySeries.filter((series) => series.kind === kind)
    }))
    .filter((group) => group.series.length > 0);

  $: historyHasStats = historyPoints.length >= 2;

  function historyMetricLabel(key: string): string {
    const [family, ...rest] = key.split('.');
    const tail = rest.join('.');
    if (family === 'cpu') {
      if (tail === 'usage_percent') return 'CPU usage';
      if (tail.startsWith('load')) return `Load (${tail.slice(4)} min)`;
      if (tail === 'temperature_c') return 'CPU temperature';
    }
    if (family === 'memory') {
      if (tail === 'usage_percent') return 'Memory usage';
      if (tail === 'used_bytes') return 'Memory used';
      if (tail === 'available_bytes') return 'Memory available';
      if (tail === 'total_bytes') return 'Memory total';
      if (tail === 'swap_used_bytes') return 'Swap used';
    }
    if (family === 'network') {
      const [iface, metric] = rest;
      return `Network ${iface} ${metric?.startsWith('rx') ? 'receive' : 'transmit'}`;
    }
    if (family === 'zfs') {
      return `ZFS ${rest[0] ?? ''} ${(rest[1] ?? '').replace('_bytes', '').replace('_percent', '')}`.trim();
    }
    if (family === 'disk') {
      const [device, metric] = rest;
      return `Disk ${device} ${metric === 'wear_percent' ? 'wear' : 'temperature'}`;
    }
    return key;
  }

  function historyValue(value: number): string {
    switch (historyUnit) {
      case 'percent':
        return `${value.toFixed(1)}%`;
      case 'bytes':
        return formatBytes(value);
      case 'bytes_per_s':
        return formatBytesPerSec(value);
      case 'celsius':
        return `${value.toFixed(1)} °C`;
      case 'load':
        return value.toFixed(2);
      default:
        return value.toFixed(2);
    }
  }

  async function loadHistoryMeta() {
    try {
      const res = await fetch('/api/reports');
      if (res.status === 401 || res.status === 403) return;
      historyVisible = true;
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const meta = (await res.json()) as ReportMetadata;
      historyMeta = meta;
      historySeries = Array.isArray(meta.series) ? meta.series : [];
      if (!historySeries.some((series) => series.key === historyMetric)) {
        historyMetric =
          historySeries.find((series) => series.key === 'cpu.usage_percent')?.key ??
          historySeries[0]?.key ??
          '';
      }
      if (!meta.available) {
        historyError = meta.error || 'Historical reporting is unavailable.';
        return;
      }
      if (historyMetric) await loadHistory();
    } catch (e) {
      historyVisible = true;
      historyError = 'Failed to load reporting metadata: ' + e;
    }
  }

  async function loadHistory() {
    if (!historyVisible || !historyMetric) return;
    historyLoading = true;
    historyError = '';
    try {
      const url = `/api/reports/series?range=${historyRange}&metric=${encodeURIComponent(historyMetric)}`;
      const res = await fetch(url);
      if (res.status === 401 || res.status === 403) {
        historyVisible = false;
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as ReportResponse;
      if (!data.available) {
        historyPoints = [];
        historyError = data.error || 'Historical reporting is unavailable.';
        return;
      }
      const selected = data.series.find((series) => series.key === historyMetric) ?? data.series[0];
      historyInterval = data.sample_interval_seconds;
      historyUnit = selected?.unit ?? historySeries.find((series) => series.key === historyMetric)?.unit ?? 'percent';
      const points = selected?.points ?? [];
      historyPoints = points.map((point) => point[3]);
      historyMin = points.length ? Math.min(...points.map((point) => point[1])) : 0;
      historyMax = points.length ? Math.max(...points.map((point) => point[2])) : 0;
    } catch (e) {
      historyError = 'Failed to load history: ' + e;
    } finally {
      historyLoading = false;
    }
  }

  function setHistoryRange(value: string) {
    if (historyRange === value) return;
    historyRange = value;
    if (historyMeta?.available) loadHistory();
  }

  loadDashboard();
  refreshTimer = setInterval(loadDashboard, REFRESH_INTERVAL_MS);
  loadHistoryMeta();
  historyTimer = setInterval(() => {
    if (historyVisible && historyMetric && document.visibilityState === 'visible') loadHistory();
  }, HISTORY_REFRESH_MS);
  onDestroy(() => {
    clearInterval(refreshTimer);
    clearInterval(historyTimer);
  });
</script>

<div>
  <h1 class="text-3xl font-bold mb-2">Dashboard</h1>
  <p class="text-gray-400 mb-8">Overview of your Naslos system.</p>

  {#if loading}
    <LoadingState label="Loading dashboard..." />
  {:else}
    {#if error}
      <p role="alert" class="text-red-400 mb-4">{error}</p>
    {/if}
    <!-- Quick stats -->
    <div class="grid grid-cols-1 md:grid-cols-4 gap-4 mb-8">
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">CPU Usage</div>
        <div class="text-3xl font-bold text-naslos-link">{data.cpu?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{data.cpu?.cores || 0} cores · {formatTemperature(data.cpu?.temperatureC)}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Memory</div>
        <div class="text-3xl font-bold text-green-400">{data.memory?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{formatBytes(data.memory?.used || 0)} / {formatBytes(data.memory?.total || 0)}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Disk</div>
        <div class="text-3xl font-bold text-blue-400">{data.disk?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{formatBytes(data.disk?.used || 0)} / {formatBytes(data.disk?.total || 0)}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Uptime</div>
        <div class="text-3xl font-bold text-purple-400">{formatUptime(data.system?.uptime || 0)}</div>
        <div class="text-xs text-gray-500">{data.system?.hostname || '—'}</div>
      </div>
    </div>

    <!-- Historical reporting (FR-RPT): bounded 24 h / 7 d / 30 d views. -->
    {#if historyVisible}
      <div class="card mb-6" data-testid="history-card">
        <div class="flex flex-wrap items-start justify-between gap-3 mb-4">
          <div>
            <h2 class="text-xl font-bold">History</h2>
            <p class="text-sm text-gray-400">
              Bounded local history (24 h, 7 d, 30 d). Stored on this node, no external monitoring.
            </p>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <div
              class="inline-flex overflow-hidden rounded-lg border border-naslos-border"
              role="group"
              aria-label="History range"
            >
              {#each historyRanges as item}
                <button
                  type="button"
                  class={`px-3 py-1.5 text-sm transition-colors ${historyRange === item.value
                    ? 'bg-naslos-primary text-white'
                    : 'bg-naslos-dark text-gray-300 hover:bg-naslos-border'}`}
                  aria-pressed={historyRange === item.value}
                  on:click={() => setHistoryRange(item.value)}
                >{item.label}</button>
              {/each}
            </div>
            {#if historyGroups.length > 0}
              <label class="sr-only" for="history-metric">History metric</label>
              <select
                id="history-metric"
                class="rounded-lg border border-naslos-border bg-naslos-dark px-3 py-1.5 text-sm"
                bind:value={historyMetric}
                on:change={loadHistory}
              >
                {#each historyGroups as group}
                  <optgroup label={group.label}>
                    {#each group.series as series}
                      <option value={series.key}>{historyMetricLabel(series.key)}</option>
                    {/each}
                  </optgroup>
                {/each}
              </select>
            {/if}
          </div>
        </div>

        {#if historyLoading}
          <p class="text-sm text-gray-400">Loading history…</p>
        {:else if historyError}
          <p role="alert" class="text-sm text-red-400" data-testid="history-error">{historyError}</p>
        {:else if !historyHasStats}
          <p class="text-sm text-gray-500" data-testid="history-empty">
            No history for this range yet. The recorder writes one point per minute; check back after a minute.
          </p>
        {:else}
          <figure aria-labelledby="history-caption">
            <Sparkline values={historyPoints} height={96} />
            <figcaption id="history-caption" class="text-sm text-gray-400 mt-2">
              {historyMetricLabel(historyMetric)} over the last {historyRange} ·
              one point per {historyInterval}s ·
              min {historyValue(historyMin)} · max {historyValue(historyMax)}
            </figcaption>
          </figure>
        {/if}
      </div>
    {/if}

    <!-- CPU detail: model, temperature, load averages and per-core usage. -->
    <div class="card mb-6" data-testid="cpu-details">
      <h2 class="text-xl font-bold mb-4">CPU</h2>
      <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
        <div>
          <div class="text-sm text-gray-400">Model</div>
          <div class="font-medium">{data.cpu?.model || '—'}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Temperature</div>
          <div class="font-medium">{formatTemperature(data.cpu?.temperatureC)}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Load average</div>
          <div class="font-medium font-mono text-sm">{formatLoad(data.cpu)}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Cores</div>
          <div class="font-medium">{data.cpu?.cores || 0}</div>
        </div>
      </div>

      {#if data.cpu?.temperatureC == null}
        <p class="text-xs text-gray-500 mt-3" data-testid="cpu-temp-unavailable">
          CPU temperature is not exposed on this node.
        </p>
      {/if}

      <div class="mt-5">
        <div class="text-sm text-gray-400 mb-2">Per-core usage</div>
        {#if (data.cpu?.perCore?.length ?? 0) > 0}
          <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
            {#each data.cpu?.perCore ?? [] as core}
              <div class="bg-naslos-dark rounded-lg p-3" data-testid="cpu-core">
                <div class="flex items-center justify-between text-sm mb-1">
                  <span>Core {core.core}</span>
                  <span class="font-mono">{core.usagePercent.toFixed(1)}%</span>
                </div>
                <div class="w-full bg-naslos-border rounded-full h-2">
                  <div
                    class={`${coreBarClass(core.usagePercent)} h-2 rounded-full transition-all`}
                    style={`width: ${Math.min(100, Math.max(0, core.usagePercent))}%`}
                  ></div>
                </div>
                <div class="text-xs text-gray-500 mt-1">
                  {core.frequencyMhz ? `${core.frequencyMhz} MHz` : 'frequency —'}
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <p class="text-sm text-gray-500">Per-core data is unavailable.</p>
        {/if}
      </div>
    </div>

    <!-- ZFS Pools -->
    <div class="card mb-6">
      <h2 class="text-xl font-bold mb-4">ZFS Pools</h2>
      {#if (data.zfs?.pools?.length ?? 0) > 0}
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          {#each data.zfs?.pools ?? [] as pool}
            <div class="bg-naslos-dark rounded-lg p-4">
              <div class="flex items-center justify-between mb-2">
                <a href="/pools/{pool.name}" class="font-bold hover:text-naslos-link transition-colors">{pool.name}</a>
                <div class="flex items-center gap-2">
                  <StatusBadge state={pool.health} unknownTone="bad" />
                  <button
                    class="btn btn-sm rounded bg-red-900/30 text-red-400 hover:bg-red-900/60 transition-colors"
                    on:click={() => confirmDelete(pool.name)}
                    title="Delete pool '{pool.name}'"
                  >Delete</button>
                </div>
              </div>
              <a href="/pools/{pool.name}" class="block">
                <div class="w-full bg-naslos-border rounded-full h-2 mb-1">
                  <div class="bg-naslos-primary h-2 rounded-full" style="width: {pool.usagePercent || 0}%"></div>
                </div>
                <div class="text-xs text-gray-500">{formatBytes(pool.alloc || 0)} / {formatBytes(pool.size || 0)} ({pool.usagePercent?.toFixed(1) || 0}%)</div>
              </a>
            </div>
          {/each}
        </div>
      {:else}
        <div class="text-center py-8">
          <p class="text-gray-400 mb-4">No ZFS pools configured yet.</p>
          <a href="/disks" class="btn btn-primary">Create Your First Pool</a>
        </div>
      {/if}
    </div>

    <!-- Delete Confirmation Modal -->
    {#if deleteTarget}
      <Modal title="Delete Pool" onClose={cancelDelete}>
        <p class="text-gray-400">
          Are you sure you want to delete pool <strong class="text-naslos-strong">{deleteTarget}</strong>?
          This will destroy the pool and all data on it. This action cannot be undone.
        </p>
        <label class="block">
          <span class="text-sm text-gray-300">Type <span class="font-mono text-naslos-strong">{deleteTarget}</span> to confirm</span>
          <input class="input w-full mt-1" bind:value={deleteConfirmText} autocomplete="off" />
        </label>
        {#if deleteError}
          <p role="alert" class="text-red-400 text-sm">{deleteError}</p>
        {/if}
        <div slot="footer">
          <button class="btn btn-secondary" on:click={cancelDelete} disabled={deleting}>Cancel</button>
          <button
            class="btn btn-danger"
            on:click={deletePool}
            disabled={deleting || deleteConfirmText !== deleteTarget}
          >
            {deleting ? 'Deleting…' : 'Delete Pool'}
          </button>
        </div>
      </Modal>
    {/if}

    <!-- System Info -->
    <div class="card">
      <h2 class="text-xl font-bold mb-4">System Information</h2>
      <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
        <div>
          <div class="text-sm text-gray-400">Hostname</div>
          <div class="font-medium">{data.system?.hostname || '—'}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">OS</div>
          <div class="font-medium">{data.system?.os || 'Talos Linux'}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Uptime</div>
          <div class="font-medium">{formatUptime(data.system?.uptime || 0)}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Updated</div>
          <div class="font-medium">{formatUpdatedAt(data.updatedAt)}</div>
        </div>
      </div>

      <!-- Per-interface addresses (FR-MET-10): the IP an operator types to reach
           this node. -->
      {#if data.network?.interfaces?.length}
        <div class="mt-4 pt-4 border-t border-naslos-border">
          <div class="text-sm text-gray-400 mb-2">
            Network
            <span class="ml-2 text-xs">
              ↓ {formatBytes(data.network.bytesRecv || 0)} · ↑ {formatBytes(data.network.bytesSent || 0)}
            </span>
          </div>
          {#each addressedInterfaces as iface}
            <span class="inline-block mr-4 mb-1">
              <span class="font-mono text-sm">{iface.name}</span>
              <span class="text-sm text-gray-400 ml-1">{iface.ipAddress}</span>
            </span>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>
