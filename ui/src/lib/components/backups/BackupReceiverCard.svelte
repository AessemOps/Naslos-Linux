<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import ErrorState from '$lib/components/ErrorState.svelte';
  import { formatBytes } from '$lib/format';
  import type { ReceiverStatus } from './types';

  export let receiver: ReceiverStatus | null = null;
  export let missing = false;
  export let error = '';

  const dispatch = createEventDispatcher<{ retry: void }>();
</script>

<div class="card">
  <h2 class="mb-2 text-lg font-bold">This buddy receives</h2>
  {#if error}
    <ErrorState message={error} retry={() => dispatch('retry')} />
  {:else if missing}
    <p class="text-sm text-gray-400">
      Buddy Backup is not configured on this instance. Create the receive dataset and set
      <code class="font-mono">buddy.receiveHostPath</code> — see docs/buddy-backup.md §5.1.
    </p>
  {:else if receiver}
    <p class="text-sm text-gray-400">
      Free: {formatBytes(receiver.freeBytes)} · Stored: {formatBytes(receiver.usedBytes)}
      {#if receiver.enrollmentOpen}
        <span class="badge badge-ok ml-2">enrollment open</span>
      {/if}
    </p>
    <h3 class="mb-1 mt-3 text-sm font-bold">Peers</h3>
    {#if receiver.peers.length === 0}
      <p class="text-sm text-gray-500">No peers authorized.</p>
    {:else}
      <DataTable label="Authorized backup peers">
        <table class="w-full">
          <thead>
            <tr>
              <th scope="col">Peer</th>
              <th scope="col">Fingerprint</th>
            </tr>
          </thead>
          <tbody>
            {#each receiver.peers as peer (peer.name)}
              <tr>
                <td class="truncate" title={peer.name}>{peer.name}</td>
                <td class="truncate font-mono text-gray-400" title={peer.fingerprint}>{peer.fingerprint}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </DataTable>
    {/if}
    <h3 class="mb-1 mt-3 text-sm font-bold">Stored backups</h3>
    {#if receiver.backups.length === 0}
      <p class="text-sm text-gray-500">Nothing stored yet.</p>
    {:else}
      <DataTable label="Backup chains stored on this receiver">
        <table class="w-full">
          <thead>
            <tr>
              <th scope="col">Source</th>
              <th scope="col">Chain</th>
              <th scope="col" class="text-right">Stored</th>
            </tr>
          </thead>
          <tbody>
            {#each receiver.backups as backup (backup.source + backup.chain)}
              <tr>
                <td class="truncate font-mono" title={backup.source}>{backup.source}</td>
                <td class="truncate font-mono" title={backup.chain}>{backup.chain}</td>
                <td class="text-right font-mono">{formatBytes(backup.storedBytes ?? 0)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </DataTable>
    {/if}
  {:else}
    <p class="text-sm text-gray-500">Receiver status is unavailable.</p>
  {/if}
</div>
