<script lang="ts">
  // Historical reporting (FR-RPT) card. Admin-only: the API gates /api/reports
  // on the owner router, so a 401/403 hides the whole section instead of
  // surfacing an error on the dashboard. Bounded 24 h / 7 d / 30 d views.
  import { onDestroy, onMount } from 'svelte';
  import Button from '../Button.svelte';
  import Skeleton from '../Skeleton.svelte';
  import Sparkline from '../Sparkline.svelte';
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
  let historyRequest = 0;
  let historyTimer: ReturnType<typeof setInterval> | null = null;
  let destroyed = false;

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
      if (tail === 'arc_bytes') return 'ZFS ARC used';
    }
    if (family === 'network') {
      const metric = rest[rest.length - 1] ?? '';
      const iface = rest.slice(0, -1).join('.');
      return `Network ${iface} ${metric.startsWith('rx') ? 'receive' : 'transmit'}`;
    }
    if (family === 'zfs') {
      const metric = rest[rest.length - 1] ?? '';
      const pool = rest.slice(0, -1).join('.');
      const label = metric.replace('_bytes', '').replace('_percent', '');
      return `ZFS ${pool} ${label}`.trim();
    }
    if (family === 'disk') {
      const metric = rest[rest.length - 1] ?? '';
      const device = rest.slice(0, -1).join('.');
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
      if (destroyed) return;
      if (res.status === 401 || res.status === 403) return;
      historyVisible = true;
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const meta = (await res.json()) as ReportMetadata;
      if (destroyed) return;
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
      if (destroyed) return;
      historyVisible = true;
      historyError = 'Failed to load reporting metadata: ' + e;
    }
  }

  async function loadHistory() {
    if (!historyVisible || !historyMetric) return;
    // Capture the request identity: a slower response for an older range or
    // metric must never overwrite the current selection.
    const request = ++historyRequest;
    const range = historyRange;
    const metric = historyMetric;
    // A background refresh with a chart already on screen keeps the chart; the
    // skeleton is only for the first load of a range/metric.
    const hadStats = historyPoints.length >= 2;
    if (!hadStats) historyLoading = true;
    historyError = '';
    try {
      const url = `/api/reports/series?range=${range}&metric=${encodeURIComponent(metric)}`;
      const res = await fetch(url);
      if (destroyed || request !== historyRequest) return;
      if (res.status === 401 || res.status === 403) {
        historyVisible = false;
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as ReportResponse;
      if (destroyed || request !== historyRequest) return;
      if (!data.available) {
        historyPoints = [];
        historyError = data.error || 'Historical reporting is unavailable.';
        return;
      }
      const selected = data.series.find((series) => series.key === metric);
      if (!selected) {
        historyPoints = [];
        historyInterval = data.sample_interval_seconds;
        historyError = 'The selected history metric is no longer available.';
        return;
      }
      historyInterval = data.sample_interval_seconds;
      historyUnit = selected.unit;
      const points = selected.points;
      historyPoints = points.map((point) => point[3]);
      historyMin = points.length ? Math.min(...points.map((point) => point[1])) : 0;
      historyMax = points.length ? Math.max(...points.map((point) => point[2])) : 0;
    } catch (e) {
      if (!destroyed && request === historyRequest) historyError = 'Failed to load history: ' + e;
    } finally {
      if (!destroyed && request === historyRequest) historyLoading = false;
    }
  }

  function setHistoryRange(value: string) {
    if (historyRange === value) return;
    historyRange = value;
    // Never label the previous range's points with the new range while the
    // replacement loads.
    historyPoints = [];
    // Keep the "reporting unavailable" message: clearing it would fall
    // through to a misleading empty chart.
    if (historyMeta?.available) {
      historyError = '';
      loadHistory();
    }
  }

  function setHistoryMetric() {
    historyPoints = [];
    historyError = '';
    loadHistory();
  }

  onMount(() => {
    loadHistoryMeta();
    historyTimer = setInterval(() => {
      if (historyVisible && historyMetric && document.visibilityState === 'visible') loadHistory();
    }, HISTORY_REFRESH_MS);
  });

  onDestroy(() => {
    destroyed = true;
    if (historyTimer) clearInterval(historyTimer);
  });
</script>

{#if historyVisible}
  <div class="card mb-6" data-testid="history-card">
    <div class="mb-4 flex flex-wrap items-start justify-between gap-3">
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
              class={`px-3 py-1.5 text-sm transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-naslos-accent ${historyRange === item.value
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
            on:change={setHistoryMetric}
          >
            {#each historyGroups as group}
              <optgroup label={group.label}>
                {#each group.series as series (series.key)}
                  <option value={series.key}>{historyMetricLabel(series.key)}</option>
                {/each}
              </optgroup>
            {/each}
          </select>
        {/if}
      </div>
    </div>

    {#if historyLoading && !historyHasStats}
      <div aria-hidden="true">
        <Skeleton height="h-24" />
      </div>
      <span class="sr-only" role="status" aria-busy="true">Loading history…</span>
    {:else if historyError && !historyHasStats}
      <div data-testid="history-error">
        <p role="alert" class="text-sm text-red-300">{historyError}</p>
        {#if historyMeta?.available}
          <div class="mt-2">
            <Button variant="secondary" size="sm" on:click={loadHistory}>Retry</Button>
          </div>
        {/if}
      </div>
    {:else if !historyHasStats}
      <p class="text-sm text-gray-500" data-testid="history-empty">
        No history for this range yet. The recorder writes one point per minute; check back after a minute.
      </p>
    {:else}
      {#if historyError}
        <div class="callout callout-bad mb-3" role="alert">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <span class="text-sm">History refresh failed; showing the last points. {historyError}</span>
            <Button variant="secondary" size="sm" on:click={loadHistory}>Retry</Button>
          </div>
        </div>
      {/if}
      <figure aria-labelledby="history-caption">
        <Sparkline values={historyPoints} height={96} />
        <figcaption id="history-caption" class="mt-2 text-sm text-gray-400">
          {historyMetricLabel(historyMetric)} over the last {historyRange} ·
          one point per {historyInterval}s ·
          min {historyValue(historyMin)} · max {historyValue(historyMax)}
        </figcaption>
      </figure>
    {/if}
  </div>
{/if}
