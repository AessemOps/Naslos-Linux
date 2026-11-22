<script lang="ts">
  // VM detail drawer: live state, lifecycle actions, snapshots, forwards and
  // the noVNC console.
  import VmConsole from './VmConsole.svelte';
  import { modal } from '$lib/actions/modal';
  import { onDestroy } from 'svelte';

  export let vm: any | null = null; // the live VmView
  export let close: () => void = () => {};
  export let action: (name: string, verb: string) => Promise<string | null>;
  export let toggleAutostart: (vm: any) => Promise<string | null>;
  export let snapshot: (name: string, snapName: string) => Promise<string | null>;
  export let restore: (name: string, snapName: string) => Promise<string | null>;

  let snapshots: any[] = [];
  let snapshotsLoading = false;
  let snapshotsError = '';
  let snapName = '';
  let busy = false;
  let error = '';
  let notice = '';
  // The page's 5s polling replaces the `vm` object wholesale; reload the
  // snapshot list only when a different VM opens (and after snapshot
  // actions), never from the polling path.
  let snapshotsFor = '';

  $: if (vm && vm.name !== snapshotsFor) {
    snapshotsFor = vm.name;
    error = '';
    notice = '';
    loadSnapshots();
  }

  // Explicitly refresh after actions; the poll never does.
  function reloadSnapshots() {
    snapshotsFor = vm?.name ?? '';
    loadSnapshots();
  }

  onDestroy(() => {
    snapshotsFor = '';
  });

  async function loadSnapshots() {
    if (!vm) return;
    snapshotsLoading = true;
    snapshotsError = '';
    try {
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(vm.name)}/snapshots`);
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      snapshots = Array.isArray(data) ? data : [];
    } catch (e: any) {
      snapshots = [];
      snapshotsError = e?.message || 'Failed to load snapshots.';
    } finally {
      snapshotsLoading = false;
    }
  }

  async function run(verb: string, fn?: () => Promise<string | null>) {
    if (busy) return;
    busy = true;
    error = '';
    notice = '';
    try {
      const err = fn ? await fn() : await action(vm.name, verb);
      if (err) error = err;
      else notice = messageFor(verb);
    } catch (e: any) {
      error = e.message;
    } finally {
      busy = false;
    }
  }

  function messageFor(verb: string): string {
    switch (verb) {
      case 'start': return 'Start requested; the console works once the VM is running.';
      case 'stop': return 'Graceful ACPI shutdown requested.';
      case 'force-stop': return 'VM terminated.';
      case 'restart': return 'Restart requested.';
      default: return 'Done.';
    }
  }

  async function onSnapshot() {
    const name = snapName.trim() || `snap-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`;
    const err = await snapshot(vm.name, name);
    if (err) {
      error = err;
    } else {
      snapName = '';
      await loadSnapshots();
    }
  }

  async function onRestore(snap: string) {
    if (!confirm(`Roll ${vm.name} back to ${snap}? Snapshots taken after it are deleted.`)) return;
    const err = await restore(vm.name, snap);
    if (err) {
      error = err;
    } else {
      notice = `Restored ${snap}.`;
      await loadSnapshots();
    }
  }

  function stateBadge(state: string): string {
    switch (state) {
      case 'running': return 'text-green-400';
      case 'stopped': return 'text-gray-400';
      default: return 'text-yellow-400';
    }
  }
</script>

{#if vm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="fixed inset-0 bg-black/60 z-40 flex items-start justify-end overflow-y-auto" use:modal={{ onClose: close }} on:click|self={close}>
    <div class="card w-full max-w-3xl min-h-full rounded-none">
      <div class="flex items-center justify-between mb-4">
        <div>
          <h2 class="text-xl font-bold">{vm.name}</h2>
          <p class="text-sm {stateBadge(vm.state)}">
            {vm.state}{vm.pid ? ` · pid ${vm.pid}` : ''}
            {#if vm.unexpectedExit} · <span class="text-red-400">exited unexpectedly</span>{/if}
          </p>
        </div>
        <button class="btn btn-secondary" on:click={close}>Close</button>
      </div>

      {#if error}
        <div role="alert" class="card mb-4 border-red-700 bg-red-900/20 text-red-300 text-sm">{error}</div>
      {/if}
      {#if notice}
        <div class="card mb-4 border-green-800 bg-green-900/20 text-green-300 text-sm">{notice}</div>
      {/if}
      {#if vm.lastError}
        <div class="card mb-4 border-yellow-700 bg-yellow-900/10 text-yellow-300 text-sm">
          Last error: {vm.lastError}
        </div>
      {/if}

      <div class="flex gap-2 flex-wrap mb-4">
        <button class="btn btn-primary" on:click={() => run('start')} disabled={busy || vm.state === 'running'}>Start</button>
        <button class="btn btn-secondary" on:click={() => run('stop')} disabled={busy || vm.state !== 'running'}>Stop</button>
        <button class="btn btn-secondary" on:click={() => run('force-stop')} disabled={busy || vm.state !== 'running'}>Force stop</button>
        <button class="btn btn-secondary" on:click={() => run('restart')} disabled={busy}>Restart</button>
        <button class="btn btn-secondary" on:click={() => run('autostart', () => toggleAutostart(vm))} disabled={busy}>
          Autostart: {vm.autostart ? 'on' : 'off'}
        </button>
      </div>

      <div class="grid grid-cols-2 md:grid-cols-4 gap-3 mb-6">
        <div class="card p-3">
          <p class="text-xs text-gray-500">vCPU</p>
          <p class="text-lg font-semibold">{vm.cpu}{#if vm.state === 'running'} · {vm.cpuPercent?.toFixed(1) ?? '0'}%{/if}</p>
        </div>
        <div class="card p-3">
          <p class="text-xs text-gray-500">Memory</p>
          <p class="text-lg font-semibold">{vm.memMiB} MiB{#if vm.state === 'running' && vm.memUsedMiB !== undefined} · {vm.memUsedMiB} MiB used{/if}</p>
        </div>
        <div class="card p-3">
          <p class="text-xs text-gray-500">Disk</p>
          <p class="text-lg font-semibold">{vm.disk.sizeGB} GB {vm.disk.format} · {vm.disk.bus}</p>
        </div>
        <div class="card p-3">
          <p class="text-xs text-gray-500">Network</p>
          <p class="text-lg font-semibold">
            {#if vm.net.mode === 'nat'}NAT{:else}LAN on {vm.net.nic}{/if}
          </p>
        </div>
      </div>

      {#if vm.net.mode === 'nat' && vm.net.forwards?.length}
        <h3 class="text-sm font-semibold text-gray-300 mb-2">Port forwards (reachable on the node)</h3>
        <div class="card mb-6 overflow-x-auto p-0">
          <table class="w-full text-sm">
            <tbody>
              {#each vm.net.forwards as fwd}
                <tr class="border-b border-naslos-border/50">
                  <td class="px-4 py-2">node:{fwd.host} → guest:{fwd.dst}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}

      <h3 class="text-sm font-semibold text-gray-300 mb-2">Console</h3>
      <div class="mb-6">
        <VmConsole vm={vm} />
      </div>

      <h3 class="text-sm font-semibold text-gray-300 mb-2">Snapshots ({vm.pool}/vms/{vm.name})</h3>
      <div class="flex items-end gap-2 mb-3 flex-wrap">
        <div>
          <label class="label" for="snap-name">Name</label>
          <input id="snap-name" class="input" bind:value={snapName} placeholder="defaults to snap-<timestamp>" />
        </div>
        <button class="btn btn-primary" on:click={onSnapshot} disabled={busy}>Snapshot now</button>
      </div>
      <p class="text-xs text-gray-500 mb-3">
        Snapshots are ordinary ZFS snapshots of the VM dataset — Buddy backs them up like any dataset
        (crash-consistent while the VM runs). Restoring requires the VM stopped.
      </p>
      {#if snapshotsLoading}
        <p class="text-sm text-gray-400">Loading snapshots…</p>
      {:else if snapshotsError}
        <div role="alert" class="card mb-6 border-red-700 bg-red-900/20 text-red-300 text-sm">
          <p>Could not load snapshots: {snapshotsError}</p>
          <button class="btn btn-secondary text-sm mt-3" on:click={loadSnapshots}>Retry</button>
        </div>
      {:else}
        <div class="card overflow-x-auto p-0 mb-6">
          <table class="w-full text-sm">
            <tbody>
              {#each snapshots as snap}
                <tr class="border-b border-naslos-border/50">
                  <td class="px-4 py-2 font-mono text-xs">{snap.name}</td>
                  <td class="px-4 py-2 text-gray-400">{snap.created ?? ''}</td>
                  <td class="px-4 py-2 text-right">
                    <button class="btn btn-secondary" on:click={() => onRestore(snap.name)}
                      disabled={vm.state === 'running'} title={vm.state === 'running' ? 'Stop the VM first' : ''}>
                      Restore
                    </button>
                  </td>
                </tr>
              {:else}
                <tr><td class="px-4 py-3 text-gray-500" colspan="3">No snapshots yet.</td></tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}

      <div class="text-xs text-gray-500 space-y-1">
        <p>Firmware: {vm.firmware === 'uefi' ? 'UEFI (OVMF)' : 'BIOS (SeaBIOS)'}{#if vm.firmware === 'uefi'} · Secure Boot {vm.secureBoot ? 'on' : 'off'}{/if} · dataset: {vm.pool}/vms/{vm.name}</p>
        {#if vm.iso}<p>Install ISO: {vm.iso}</p>{/if}
        {#if vm.disk.sourceImage}<p>Seeded from: {vm.disk.sourceImage}</p>{/if}
        {#if vm.cloudInit}<p>cloud-init: user {vm.cloudInit.user}, {vm.cloudInit.sshKeys?.length ?? 0} SSH key(s)</p>{/if}
      </div>
    </div>
  </div>
{/if}
