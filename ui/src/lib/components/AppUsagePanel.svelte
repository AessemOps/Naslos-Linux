<script lang="ts">
  import Sparkline from './Sparkline.svelte';
  import { formatBytes, formatBytesPerSec, formatMillicores } from '$lib/format';
  import type { AppUsage, AppUsageHistory } from '$lib/appTypes';

  export let usage: AppUsage | undefined = undefined;
  export let history: AppUsageHistory | undefined = undefined;

  function collecting(values: number[] | undefined): boolean {
    return !values || values.length < 2;
  }
</script>

<section>
  <div class="flex items-center justify-between mb-2">
    <h3 class="text-xs uppercase tracking-wide text-gray-500">Usage</h3>
    <span class="text-xs text-gray-500">live · updates every 5s</span>
  </div>
  {#if !usage}
    <p class="text-xs text-gray-500">Usage is unavailable right now.</p>
  {:else}
    <div class="grid grid-cols-2 gap-3">
      <div class="rounded-lg border border-naslos-border p-3">
        <div class="text-xs text-gray-400">CPU</div>
        <div class="text-lg font-semibold tabular-nums">{formatMillicores(usage.cpuMillicores)}</div>
        {#if collecting(history?.cpu)}
          <p class="text-xs text-gray-500">collecting…</p>
        {:else}
          <Sparkline values={history?.cpu ?? []} />
        {/if}
      </div>
      <div class="rounded-lg border border-naslos-border p-3">
        <div class="text-xs text-gray-400">RAM</div>
        <div class="text-lg font-semibold tabular-nums">{formatBytes(usage.memoryBytes)}</div>
        {#if collecting(history?.memory)}
          <p class="text-xs text-gray-500">collecting…</p>
        {:else}
          <Sparkline values={history?.memory ?? []} />
        {/if}
      </div>
      <div class="rounded-lg border border-naslos-border p-3">
        <div class="text-xs text-gray-400">Network in</div>
        <div class="text-lg font-semibold tabular-nums">{formatBytesPerSec(usage.networkRxBytesPerSec)}</div>
        {#if collecting(history?.rx)}
          <p class="text-xs text-gray-500">collecting…</p>
        {:else}
          <Sparkline values={history?.rx ?? []} />
        {/if}
      </div>
      <div class="rounded-lg border border-naslos-border p-3">
        <div class="text-xs text-gray-400">Network out</div>
        <div class="text-lg font-semibold tabular-nums">{formatBytesPerSec(usage.networkTxBytesPerSec)}</div>
        {#if collecting(history?.tx)}
          <p class="text-xs text-gray-500">collecting…</p>
        {:else}
          <Sparkline values={history?.tx ?? []} />
        {/if}
      </div>
    </div>
    <p class="text-xs text-gray-500 mt-2 tabular-nums">{usage.pods} pods sampled</p>
  {/if}
</section>
