<script lang="ts">
  // CPU detail block: model, temperature, load averages and one bar per logical
  // core. The per-core count and the unavailable-temperature note are part of
  // the dashboard contract (data-testid cpu-core / cpu-temp-unavailable).
  import type { CpuInfo } from './types';
  import { usageTone } from './thresholds';

  export let cpu: CpuInfo | undefined = undefined;

  function formatTemperature(value?: number | null): string {
    return value === undefined || value === null || Number.isNaN(value)
      ? '—'
      : `${value.toFixed(1)} °C`;
  }

  function formatLoad(value?: CpuInfo): string {
    if (!value) return '—';
    return `${(value.loadAvg1 ?? 0).toFixed(2)} / ${(value.loadAvg5 ?? 0).toFixed(2)} / ${(value.loadAvg15 ?? 0).toFixed(2)}`;
  }

  function coreBarClass(usage: number): string {
    const tone = usageTone(usage);
    if (tone === 'bad') return 'bg-red-500';
    if (tone === 'warn') return 'bg-amber-400';
    return 'bg-naslos-primary';
  }
</script>

<div class="card mb-6" data-testid="cpu-details">
  <h2 class="text-xl font-bold mb-4">CPU</h2>
  <dl class="grid grid-cols-2 gap-4 md:grid-cols-4">
    <div>
      <dt class="text-sm text-gray-400">Model</dt>
      <dd class="font-medium">{cpu?.model || '—'}</dd>
    </div>
    <div>
      <dt class="text-sm text-gray-400">Temperature</dt>
      <dd class="font-medium">{formatTemperature(cpu?.temperatureC)}</dd>
    </div>
    <div>
      <dt class="text-sm text-gray-400">Load average</dt>
      <dd class="font-medium font-mono text-sm">{formatLoad(cpu)}</dd>
    </div>
    <div>
      <dt class="text-sm text-gray-400">Cores</dt>
      <dd class="font-medium">{cpu?.cores || 0}</dd>
    </div>
  </dl>

  {#if cpu?.temperatureC == null}
    <p class="mt-3 text-xs text-gray-500" data-testid="cpu-temp-unavailable">
      CPU temperature is not exposed on this node.
    </p>
  {/if}

  <div class="mt-5">
    <div class="mb-2 text-sm text-gray-400">Per-core usage</div>
    {#if (cpu?.perCore?.length ?? 0) > 0}
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {#each cpu?.perCore ?? [] as core (core.core)}
          <div class="rounded-lg bg-naslos-dark p-3" data-testid="cpu-core">
            <div class="mb-1 flex items-center justify-between text-sm">
              <span>Core {core.core}</span>
              <span class="font-mono">{core.usagePercent.toFixed(1)}%</span>
            </div>
            <div
              class="h-2 w-full rounded-full bg-naslos-border"
              role="progressbar"
              aria-label={`Core ${core.core} usage`}
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={Math.round(core.usagePercent)}
            >
              <div
                class={`${coreBarClass(core.usagePercent)} h-2 rounded-full transition-all`}
                style={`width: ${Math.min(100, Math.max(0, core.usagePercent))}%`}
              ></div>
            </div>
            <div class="mt-1 text-xs text-gray-500">
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
