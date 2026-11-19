<script lang="ts">
  import { onMount } from 'svelte';
  import VmWizard from '$lib/components/VmWizard.svelte';
  import VmDetail from '$lib/components/VmDetail.svelte';
  import VmImages from '$lib/components/VmImages.svelte';

  interface VmView {
    name: string;
    pool: string;
    cpu: number;
    memMiB: number;
    autostart: boolean;
    desired: string;
    firmware: string;
    disk: { sizeGB: number; format: string; bus: string; sourceImage?: string | null };
    net: { mode: string; forwards: Array<{ host: number; dst: number }>; nic?: string | null };
    cloudInit?: { hostname: string; user: string; sshKeys: string[] } | null;
    iso?: string | null;
    state: string;
    pid?: number | null;
    cpuPercent?: number | null;
    memUsedMiB?: number | null;
    vncPort?: number | null;
    unexpectedExit: boolean;
    lastError: string;
  }

  let vms: VmView[] = [];
  let pools: string[] = [];
  let capabilities: any = null;
  let capsError = '';
  let featureError = '';
  let loading = true;

  let wizardOpen = false;
  let editing: VmView | null = null;
  let selected: VmView | null = null;
  let imagesOpen = false;
  let imagePool = '';
  let imagesRefreshKey = 0;

  let error = '';
  let timer: ReturnType<typeof setInterval> | null = null;

  async function loadVms() {
    try {
      const res = await fetch('/api/virt/vms');
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      vms = Array.isArray(data) ? data : [];
      featureError = '';
    } catch (e: any) {
      featureError = e.message;
    }
  }

  async function loadPools() {
    try {
      const res = await fetch('/api/volumes/zfs');
      if (!res.ok) return;
      const data = await res.json();
      pools = (Array.isArray(data) ? data : []).map((p: any) => p.name);
      if (!imagePool && pools.length > 0) imagePool = pools[0];
    } catch {
      // The wizard reports the empty pool list.
    }
  }

  async function loadCaps() {
    try {
      const res = await fetch('/api/virt/capabilities');
      if (!res.ok) {
        // 503: the feature is wired off or the supervisor is unreachable.
        featureError = 'The QEMU supervisor is not reachable on this install.';
        return;
      }
      capabilities = await res.json();
    } catch (e: any) {
      capsError = e.message;
    }
  }

  async function refresh() {
    await Promise.all([loadVms()]);
    if (selected) {
      // Keep the drawer's VM in step with the list.
      selected = vms.find((v) => v.name === selected!.name) ?? selected;
    }
  }

  onMount(() => {
    loadCaps();
    loadPools();
    refresh().then(() => (loading = false));
    timer = setInterval(() => {
      if (document.visibilityState === 'visible') refresh();
    }, 5000);
    return () => {
      if (timer) clearInterval(timer);
    };
  });

  async function createOrUpdate(payload: any, isEdit: boolean): Promise<string | null> {
    try {
      const res = await fetch(isEdit ? `/api/virt/vms/${encodeURIComponent(payload.name)}` : '/api/virt/vms', {
        method: isEdit ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      const data = await res.json();
      if (!res.ok) return data.error || `HTTP ${res.status}`;
      await refresh();
      return null;
    } catch (e: any) {
      return e.message;
    }
  }

  async function action(name: string, verb: string): Promise<string | null> {
    try {
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(name)}/${verb}`, { method: 'POST' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) return data.error || `HTTP ${res.status}`;
      await refresh();
      return null;
    } catch (e: any) {
      return e.message;
    }
  }

  async function toggleAutostart(vm: VmView): Promise<string | null> {
    return createOrUpdate({ ...vm, autostart: !vm.autostart }, true);
  }

  async function snapshot(name: string, snapName: string): Promise<string | null> {
    try {
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(name)}/snapshots`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: snapName })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) return data.error || `HTTP ${res.status}`;
      return null;
    } catch (e: any) {
      return e.message;
    }
  }

  async function restore(name: string, snapName: string): Promise<string | null> {
    try {
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(name)}/snapshots/${encodeURIComponent(snapName)}/restore`, {
        method: 'POST'
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) return data.error || `HTTP ${res.status}`;
      return null;
    } catch (e: any) {
      return e.message;
    }
  }

  async function removeVm(vm: VmView) {
    if (!confirm(`Delete ${vm.name}? Its dataset (${vm.pool}/vms/${vm.name}) and everything on it is destroyed.`)) return;
    error = '';
    try {
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(vm.name)}`, { method: 'DELETE' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      if (selected?.name === vm.name) selected = null;
      await refresh();
    } catch (e: any) {
      error = e.message;
    }
  }

  function stateBadge(state: string): string {
    switch (state) {
      case 'running': return 'bg-green-900/40 text-green-300 border-green-700';
      case 'stopped': return 'bg-gray-800/60 text-gray-300 border-gray-600';
      default: return 'bg-yellow-900/40 text-yellow-300 border-yellow-700';
    }
  }

  async function openCreate() {
    if (pools.length === 0) await loadPools();
    editing = null;
    wizardOpen = true;
  }

  async function openEdit(vm: VmView) {
    editing = vm;
    wizardOpen = true;
  }

  async function openImages(pool?: string) {
    if (pool) imagePool = pool;
    imagesOpen = !imagesOpen;
    if (imagesOpen) imagesRefreshKey++;
  }
</script>

<div class="max-w-6xl mx-auto">
  <div class="flex items-start justify-between gap-4 mb-4">
    <div>
      <h1 class="text-3xl font-bold mb-2">Virtual Machines</h1>
      <p class="text-gray-400">
        A first-party QEMU/KVM hypervisor on the node: VM datasets live under
        <code class="text-xs">&lt;pool&gt;/vms/&lt;name&gt;</code>, the console is a browser VNC, and
        snapshots are ordinary ZFS snapshots.
      </p>
    </div>
    <div class="flex gap-2">
      <button class="btn btn-secondary" on:click={() => openImages()}>
        {imagesOpen ? 'Hide images' : 'Images'}
      </button>
      <button class="btn btn-primary" on:click={openCreate} disabled={capabilities === null && !featureError}>
        New VM
      </button>
    </div>
  </div>

  {#if capabilities && !capabilities.kvm}
    <div class="card mb-4 border-yellow-700 bg-yellow-900/10 text-yellow-300 text-sm">
      This node has no usable <strong>KVM</strong> (VT-x/AMD-V missing or disabled in the BIOS).
      VMs would run emulated (very slow); vmd stays idle until the hardware is enabled.
    </div>
  {/if}
  {#if capabilities && !capabilities.macvtap}
    <div class="card mb-4 border-yellow-700 bg-yellow-900/10 text-yellow-300 text-sm">
      macvtap is unavailable on this kernel, so LAN networking is disabled for new VMs; NAT with port
      forwards still works.
    </div>
  {/if}
  {#if featureError}
    <div role="alert" class="card mb-4 border-red-700 bg-red-900/20 text-red-300 text-sm">{featureError}</div>
  {/if}
  {#if error}
    <div role="alert" class="card mb-4 border-red-700 bg-red-900/20 text-red-300 text-sm">{error}</div>
  {/if}

  {#if imagesOpen}
    <div class="mb-6">
      <div class="flex items-end gap-3 mb-3 flex-wrap">
        <div>
          <label class="label" for="img-pool">Pool</label>
          <select id="img-pool" class="input" bind:value={imagePool}>
            {#each pools as p}
              <option value={p}>{p}</option>
            {/each}
          </select>
        </div>
        <button class="btn btn-secondary" on:click={() => (imagesRefreshKey++)}>Refresh</button>
      </div>
      <VmImages pool={imagePool} refreshKey={imagesRefreshKey} />
    </div>
  {/if}

  {#if loading}
    <p class="text-sm text-gray-400">Loading virtual machines…</p>
  {:else}
    <div class="card overflow-x-auto p-0">
      <table class="w-full text-sm">
        <thead>
          <tr class="text-left text-gray-400 border-b border-naslos-border">
            <th class="px-4 py-3">Name</th>
            <th class="px-4 py-3">State</th>
            <th class="px-4 py-3">vCPU / Memory</th>
            <th class="px-4 py-3">Disk</th>
            <th class="px-4 py-3">Network</th>
            <th class="px-4 py-3">Autostart</th>
            <th class="px-4 py-3"></th>
          </tr>
        </thead>
        <tbody>
          {#each vms as vm (vm.name)}
            <tr class="border-b border-naslos-border/50 hover:bg-naslos-border/20 cursor-pointer"
              on:click={() => (selected = vm)}>
              <td class="px-4 py-3 font-medium">
                {vm.name}
                {#if vm.unexpectedExit}<span class="text-red-400" title={vm.lastError}> ⚠</span>{/if}
              </td>
              <td class="px-4 py-3">
                <span class="px-2 py-0.5 rounded-full border text-xs {stateBadge(vm.state)}">{vm.state}</span>
              </td>
              <td class="px-4 py-3">
                {vm.cpu}{#if vm.state === 'running'} ({vm.cpuPercent?.toFixed(1) ?? '0'}%){/if}
                / {vm.memMiB} MiB{#if vm.state === 'running' && vm.memUsedMiB !== undefined} ({vm.memUsedMiB} used){/if}
              </td>
              <td class="px-4 py-3">{vm.disk.sizeGB} GB {vm.disk.format}</td>
              <td class="px-4 py-3">
                {#if vm.net.mode === 'nat'}
                  NAT{#if vm.net.forwards.length} · {vm.net.forwards.length} forward(s){/if}
                {:else}
                  LAN on {vm.net.nic}
                {/if}
              </td>
              <td class="px-4 py-3">{vm.autostart ? 'yes' : 'no'}</td>
              <td class="px-4 py-3 text-right whitespace-nowrap">
                <button class="btn btn-secondary" on:click|stopPropagation={() => (selected = vm)}>Open</button>
                <button class="btn btn-secondary ml-1" on:click|stopPropagation={() => openEdit(vm)}>Edit</button>
                <button class="btn btn-danger ml-1" on:click|stopPropagation={() => removeVm(vm)}>Delete</button>
              </td>
            </tr>
          {:else}
            <tr>
              <td class="px-4 py-6 text-gray-500" colspan="7">
                {featureError ? 'The VM list is unavailable.' : 'No virtual machines yet — create one, or import an ISO first.'}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if capabilities}
      <p class="text-xs text-gray-500 mt-2">
        Node: {capabilities.cpuModel || 'unknown CPU'} · KVM {capabilities.kvm ? 'yes' : 'no'}
        {capabilities.nested ? ' (nested)' : ''} · tun {capabilities.tun ? 'yes' : 'no'} ·
        VNC range 5901+{#if capabilities.nics} · NICs {capabilities.nics.join(', ')}{/if}
      </p>
    {/if}
  {/if}
</div>

<VmWizard open={wizardOpen} vm={editing} pools={pools} capabilities={capabilities}
  close={() => (wizardOpen = false)} submit={createOrUpdate} />

{#if selected}
  <VmDetail vm={selected} close={() => (selected = null)} action={action}
    toggleAutostart={toggleAutostart} snapshot={snapshot} restore={restore} />
{/if}
