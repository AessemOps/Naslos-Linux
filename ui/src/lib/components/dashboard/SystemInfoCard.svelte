<script lang="ts">
  // System and network summary. Hostname keeps the label/value div pair the
  // dashboard spec reads through following-sibling::div[1]; the remaining rows
  // are a definition list.
  import { formatBytes } from '$lib/format';
  import type { NetworkInfo, SystemInfo } from './types';

  export let system: SystemInfo | undefined = undefined;
  export let network: NetworkInfo | undefined = undefined;
  export let updatedAt = '';

  $: addressedInterfaces = (network?.interfaces ?? []).filter((i) => i.ipAddress);

  function formatUptime(seconds: number): string {
    if (!seconds) return '—';
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const mins = Math.floor((seconds % 3600) / 60);
    if (days > 0) return `${days}d ${hours}h ${mins}m`;
    if (hours > 0) return `${hours}h ${mins}m`;
    return `${mins}m`;
  }

  // The zero-value Go time (0001-01-01...) means "not collected yet".
  function formatUpdatedAt(iso?: string): string {
    if (!iso || iso.startsWith('0001-01-01')) return '—';
    const d = new Date(iso);
    if (isNaN(d.getTime())) return '—';
    return d.toLocaleString();
  }

  function fmtBytes(value?: number): string {
    return value == null ? '—' : formatBytes(value);
  }
</script>

<section class="card" aria-labelledby="dashboard-system">
  <h2 id="dashboard-system" class="mb-4 text-xl font-bold">System Information</h2>

  <div class="grid grid-cols-2 gap-4 md:grid-cols-4">
    <div>
      <div class="text-sm text-gray-400">Hostname</div>
      <div class="font-medium font-mono">{system?.hostname || '—'}</div>
    </div>
    <dl class="contents">
      <div>
        <dt class="text-sm text-gray-400">OS</dt>
        <dd class="font-medium">{system?.os || 'Talos Linux'}</dd>
      </div>
      <div>
        <dt class="text-sm text-gray-400">Uptime</dt>
        <dd class="font-medium">{formatUptime(system?.uptime || 0)}</dd>
      </div>
      <div>
        <dt class="text-sm text-gray-400">Updated</dt>
        <dd class="font-medium">{formatUpdatedAt(updatedAt)}</dd>
      </div>
    </dl>
  </div>

  {#if (network?.interfaces?.length ?? 0) > 0}
    <div class="mt-4 border-t border-naslos-border pt-4">
      <div class="mb-2 text-sm text-gray-400">
        Network
        <span class="ml-2 text-xs">
          ↓ {fmtBytes(network?.bytesRecv)} · ↑ {fmtBytes(network?.bytesSent)}
        </span>
      </div>
      <div class="flex flex-wrap gap-x-4 gap-y-1">
        {#each addressedInterfaces as iface (iface.name)}
          <span class="inline-flex items-baseline gap-2">
            <span class="font-mono text-sm">{iface.name}</span>
            <span class="text-sm text-gray-400">{iface.ipAddress}</span>
          </span>
        {/each}
      </div>
    </div>
  {/if}
</section>
