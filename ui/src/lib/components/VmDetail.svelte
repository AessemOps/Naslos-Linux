<script lang="ts">
  // VM detail drawer: live state, lifecycle actions, snapshots, forwards and
  // the noVNC console.
  import { onDestroy } from 'svelte';
  import { modal } from '$lib/actions/modal';
  import Button from './Button.svelte';
  import DataTable from './DataTable.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import VmConsole from './VmConsole.svelte';
  import { confirmAction } from '$lib/stores/confirm';

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
  let busyVerb = '';
  let snapshotting = false;
  let restoring = '';
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
    busyVerb = verb;
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
      busyVerb = '';
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
    if (snapshotting) return;
    snapshotting = true;
    const name = snapName.trim() || `snap-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`;
    const err = await snapshot(vm.name, name);
    if (err) {
      error = err;
    } else {
      snapName = '';
      await loadSnapshots();
    }
    snapshotting = false;
  }

  async function onRestore(snap: string) {
    const confirmed = await confirmAction({
      title: 'Restore snapshot',
      message: `Restore ${vm.name} to snapshot ${snap}? Snapshots taken after it are deleted.`,
      confirmLabel: 'Restore',
      tone: 'danger',
      typedConfirmation: vm.name
    });
    if (!confirmed) return;
    restoring = snap;
    error = '';
    try {
      const err = await restore(vm.name, snap);
      if (err) {
        error = err;
      } else {
        notice = `Restored ${snap}.`;
        await loadSnapshots();
      }
    } finally {
      restoring = '';
    }
  }
</script>

{#if vm}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="fixed inset-0 z-50" use:modal={{ onClose: close }}>
    <div class="absolute inset-0 bg-black/60" on:click={close}></div>
    <aside
      class="absolute inset-y-0 right-0 flex w-full max-w-3xl flex-col border-l border-naslos-border bg-naslos-surface shadow-xl"
    >
      <div class="flex items-start justify-between gap-3 border-b border-naslos-border p-4">
        <div class="min-w-0">
          <h2 class="text-xl font-bold truncate">{vm.name}</h2>
          <div class="mt-1 flex flex-wrap items-center gap-2 text-sm text-gray-400">
            <StatusBadge state={vm.state} />
            {#if vm.pid}<span class="font-mono text-xs">pid {vm.pid}</span>{/if}
            {#if vm.unexpectedExit}<span class="text-red-400">exited unexpectedly</span>{/if}
          </div>
        </div>
        <Button variant="secondary" on:click={close}>Close</Button>
      </div>

      <div class="flex-1 space-y-5 overflow-y-auto p-4 sm:p-6">
        {#if error}
          <div role="alert" class="callout callout-bad mb-0">{error}</div>
        {/if}
        {#if notice}
          <div class="callout callout-ok mb-0" role="status">{notice}</div>
        {/if}
        {#if vm.lastError}
          <div class="callout callout-warn mb-0">
            Last error: {vm.lastError}
          </div>
        {/if}

        <div class="flex flex-wrap gap-2">
          <Button disabled={busy || vm.state === 'running'} loading={busyVerb === 'start'} on:click={() => run('start')}>Start</Button>
          <Button variant="secondary" disabled={busy || vm.state !== 'running'} loading={busyVerb === 'stop'} on:click={() => run('stop')}>Stop</Button>
          <Button variant="secondary" disabled={busy || vm.state !== 'running'} loading={busyVerb === 'force-stop'} on:click={() => run('force-stop')}>Force stop</Button>
          <Button variant="secondary" disabled={busy} loading={busyVerb === 'restart'} on:click={() => run('restart')}>Restart</Button>
          <Button
            variant="secondary"
            disabled={busy}
            loading={busyVerb === 'autostart'}
            on:click={() => run('autostart', () => toggleAutostart(vm))}
          >
            Autostart: {vm.autostart ? 'on' : 'off'}
          </Button>
        </div>

        <div class="grid grid-cols-2 gap-3 md:grid-cols-4">
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
          <div>
            <h3 class="text-sm font-semibold text-gray-300 mb-2">Port forwards (reachable on the node)</h3>
            <DataTable>
              <table>
                <thead>
                  <tr><th scope="col">Forward</th></tr>
                </thead>
                <tbody>
                  {#each vm.net.forwards as fwd}
                    <tr>
                      <td class="font-mono text-xs">node:{fwd.host} → guest:{fwd.dst}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </DataTable>
          </div>
        {/if}

        <div>
          <h3 class="text-sm font-semibold text-gray-300 mb-2">Console</h3>
          <VmConsole {vm} />
        </div>

        <div>
          <h3 class="text-sm font-semibold text-gray-300 mb-2">Snapshots ({vm.pool}/vms/{vm.name})</h3>
          <form class="mb-3 flex flex-wrap items-end gap-2" on:submit|preventDefault={onSnapshot}>
            <div>
              <label class="label" for="snap-name">Name</label>
              <input id="snap-name" class="input" bind:value={snapName} placeholder="defaults to snap-<timestamp>" />
            </div>
            <Button type="submit" loading={snapshotting}>Snapshot now</Button>
          </form>
          <p class="text-xs text-gray-500 mb-3">
            Snapshots are ordinary ZFS snapshots of the VM dataset — Buddy backs them up like any dataset
            (crash-consistent while the VM runs). Restoring requires the VM stopped.
          </p>
          {#if snapshotsLoading}
            <div role="status" aria-busy="true" class="space-y-2 py-1">
              <span class="sr-only">Loading snapshots…</span>
              <div class="skeleton h-3 w-48"></div>
              <div class="skeleton h-3 w-32"></div>
            </div>
          {:else if snapshotsError}
            <div role="alert" class="callout callout-bad">
              <p>Could not load snapshots: {snapshotsError}</p>
              <Button variant="secondary" size="sm" on:click={loadSnapshots}>Retry</Button>
            </div>
          {:else}
            <DataTable>
              <table>
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">Created</th>
                    <th scope="col"><span class="sr-only">Actions</span></th>
                  </tr>
                </thead>
                <tbody>
                  {#each snapshots as snap}
                    <tr>
                      <td class="font-mono text-xs">{snap.name}</td>
                      <td class="text-gray-400">{snap.created ?? ''}</td>
                      <td>
                        <div class="flex justify-end">
                          <Button
                            variant="secondary"
                            size="sm"
                            on:click={() => onRestore(snap.name)}
                            loading={restoring === snap.name}
                            disabled={(vm.state === 'running' && !restoring) || (!!restoring && restoring !== snap.name)}
                            title={vm.state === 'running' ? 'Stop the VM first' : ''}
                          >
                            Restore
                          </Button>
                        </div>
                      </td>
                    </tr>
                  {:else}
                    <tr><td class="px-4 py-3 text-gray-500" colspan="3">No snapshots yet.</td></tr>
                  {/each}
                </tbody>
              </table>
            </DataTable>
          {/if}
        </div>

        <div class="text-xs text-gray-500 space-y-1">
          <p>Firmware: {vm.firmware === 'uefi' ? 'UEFI (OVMF)' : 'BIOS (SeaBIOS)'}{#if vm.firmware === 'uefi'} · Secure Boot {vm.secureBoot ? 'on' : 'off'}{/if} · dataset: {vm.pool}/vms/{vm.name}</p>
          {#if vm.iso}<p>Install ISO: {vm.iso}</p>{/if}
          {#if vm.disk.sourceImage}<p>Seeded from: {vm.disk.sourceImage}</p>{/if}
          {#if vm.cloudInit}<p>cloud-init: user {vm.cloudInit.user}, {vm.cloudInit.sshKeys?.length ?? 0} SSH key(s)</p>{/if}
        </div>
      </div>
    </aside>
  </div>
{/if}
