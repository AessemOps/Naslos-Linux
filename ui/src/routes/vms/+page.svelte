<script lang="ts">
  import { onMount } from 'svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import VmWizard from '$lib/components/VmWizard.svelte';
  import VmDetail from '$lib/components/VmDetail.svelte';
  import VmImages from '$lib/components/VmImages.svelte';
  import { confirmAction } from '$lib/stores/confirm';
  import type { VmImage, VmPayload, VmView } from '$lib/vmTypes';

  let vms: VmView[] = [];
  let pools: string[] = [];
  let capabilities: any = null;
  let featureError = '';
  let loading = true;

  let wizardOpen = false;
  let editing: VmView | null = null;
  let selected: VmView | null = null;
  let imagesOpen = false;
  let imagePool = '';
  let imagesRefreshKey = 0;
  // The wizard's image library is per pool: selecting a different pool in the
  // create form reloads it so imported ISOs/cloud images are selectable.
  let wizardImages: VmImage[] = [];
  let wizardImagesSeq = 0;

  let error = '';
  let removing = '';
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
      featureError = `Cannot load the node capabilities: ${e.message}`;
    }
  }

  let refreshing = false;

  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      await loadVms();
      if (selected) {
        // Keep the drawer's VM in step with the list.
        selected = vms.find((v) => v.name === selected!.name) ?? selected;
      }
    } finally {
      refreshing = false;
    }
  }

  async function loadAll() {
    loading = true;
    featureError = '';
    await Promise.all([loadCaps(), loadPools(), loadVms()]);
    loading = false;
  }

  onMount(() => {
    void loadAll();
    timer = setInterval(() => {
      if (document.visibilityState === 'visible') refresh();
    }, 5000);
    return () => {
      if (timer) clearInterval(timer);
    };
  });

  async function loadWizardImages(pool: string) {
    const seq = ++wizardImagesSeq;
    if (!pool) {
      wizardImages = [];
      return;
    }
    try {
      const res = await fetch(`/api/virt/images?pool=${encodeURIComponent(pool)}`);
      if (!res.ok) {
        // Drop the previous pool's list on failure: the wizard must not
        // offer an image that does not exist in the selected pool.
        if (seq === wizardImagesSeq) wizardImages = [];
        return;
      }
      const data = await res.json();
      if (seq !== wizardImagesSeq) return;
      wizardImages = Array.isArray(data.images) ? data.images : [];
    } catch {
      if (seq === wizardImagesSeq) wizardImages = [];
      // The wizard still works with an empty library; the Images panel reports
      // the real error.
    }
  }

  async function createOrUpdate(payload: VmPayload, isEdit: boolean): Promise<string | null> {
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
    // Send only the spec fields; the live view carries state/pid/usage keys
    // the API does not persist.
    const payload: VmPayload = {
      name: vm.name,
      pool: vm.pool,
      cpu: vm.cpu,
      memMiB: vm.memMiB,
      autostart: !vm.autostart,
      firmware: vm.firmware,
      secureBoot: !!vm.secureBoot,
      desired: vm.desired,
      disk: {
        sizeGB: vm.disk.sizeGB,
        format: vm.disk.format,
        bus: vm.disk.bus,
        sourceImage: vm.disk.sourceImage ?? null
      },
      net: { mode: vm.net.mode, forwards: vm.net.forwards ?? [] },
      iso: vm.iso ?? null
    };
    if (vm.cloudInit) {
      payload.cloudInit = {
        hostname: vm.cloudInit.hostname,
        user: vm.cloudInit.user,
        sshKeys: vm.cloudInit.sshKeys ?? []
      };
    }
    return createOrUpdate(payload, true);
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
      // VmDetail gates this with a typed VM-name prompt before calling here;
      // the API requires the same name as an explicit confirmation.
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(name)}/snapshots/${encodeURIComponent(snapName)}/restore?confirm=${encodeURIComponent(name)}`, {
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
    const confirmed = await confirmAction({
      title: 'Delete virtual machine',
      message: `Delete ${vm.name}? Its dataset (${vm.pool}/vms/${vm.name}) and everything on it is destroyed.`,
      confirmLabel: 'Delete',
      tone: 'danger',
      typedConfirmation: vm.name
    });
    if (!confirmed) return;
    removing = vm.name;
    error = '';
    try {
      const res = await fetch(`/api/virt/vms/${encodeURIComponent(vm.name)}?confirm=${encodeURIComponent(vm.name)}`, { method: 'DELETE' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      if (selected?.name === vm.name) selected = null;
      await refresh();
    } catch (e: any) {
      error = e.message;
    } finally {
      removing = '';
    }
  }

  async function openCreate() {
    if (pools.length === 0) await loadPools();
    editing = null;
    wizardOpen = true;
    loadWizardImages(pools[0] ?? '');
  }

  async function openEdit(vm: VmView) {
    editing = vm;
    wizardOpen = true;
    loadWizardImages(vm.pool);
  }

  async function openImages(pool?: string) {
    if (pool) imagePool = pool;
    imagesOpen = !imagesOpen;
    if (imagesOpen) imagesRefreshKey++;
  }
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Virtual Machines"
    subtitle="A first-party QEMU/KVM hypervisor on the node: VM datasets live under <pool>/vms/<name>, the console is a browser VNC, and snapshots are ordinary ZFS snapshots."
  >
    <div slot="actions" class="flex flex-wrap gap-2">
      <Button variant="secondary" on:click={() => openImages()}>
        {imagesOpen ? 'Hide images' : 'Images'}
      </Button>
      <Button on:click={openCreate} disabled={capabilities === null && !featureError}>New VM</Button>
    </div>
  </PageHeader>

  {#if capabilities && !capabilities.kvm}
    <div class="callout callout-warn">
      This node has no usable <strong>KVM</strong> (VT-x/AMD-V missing or disabled in the BIOS).
      VMs would run emulated (very slow); vmd stays idle until the hardware is enabled.
    </div>
  {/if}
  {#if error}
    <div role="alert" class="callout callout-bad">{error}</div>
  {/if}

  {#if imagesOpen}
    <div class="mb-6">
      <div class="mb-3 flex flex-wrap items-end gap-3">
        <div>
          <label class="label" for="img-pool">Pool</label>
          <select id="img-pool" class="input" bind:value={imagePool}>
            {#each pools as p}
              <option value={p}>{p}</option>
            {/each}
          </select>
        </div>
        <Button variant="secondary" on:click={() => (imagesRefreshKey++)}>Refresh</Button>
      </div>
      <VmImages pool={imagePool} refreshKey={imagesRefreshKey} />
    </div>
  {/if}

  <AsyncState
    loading={loading}
    error={featureError}
    empty={!loading && !featureError && vms.length === 0}
    loadingText="Loading virtual machines…"
    emptyTitle="No virtual machines yet"
    emptyDetail="Create one, or import an ISO first."
    emptyIcon="monitor"
    onRetry={loadAll}
  >
    <DataTable>
      <table>
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">State</th>
            <th scope="col">vCPU / Memory</th>
            <th scope="col">Disk</th>
            <th scope="col">Network</th>
            <th scope="col">Autostart</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each vms as vm (vm.name)}
            <tr>
              <td class="font-medium">
                <button
                  type="button"
                  class="inline-flex items-center gap-1 text-left hover:underline"
                  on:click={() => (selected = vm)}
                >
                  {vm.name}
                  {#if vm.unexpectedExit}
                    <span class="text-red-400" title={vm.lastError}><Icon name="triangle-alert" size={16} /></span>
                  {/if}
                </button>
              </td>
              <td>
                <StatusBadge state={vm.state} />
              </td>
              <td class="tabular-nums whitespace-nowrap">
                {vm.cpu}{#if vm.state === 'running'}<span class="text-gray-500"> ({vm.cpuPercent?.toFixed(1) ?? '0'}%)</span>{/if}
                <span class="mx-1 text-gray-600">/</span>{vm.memMiB} MiB{#if vm.state === 'running' && vm.memUsedMiB !== undefined}<span class="text-gray-500"> ({vm.memUsedMiB} used)</span>{/if}
              </td>
              <td>{vm.disk.sizeGB} GB {vm.disk.format}</td>
              <td>
                {#if vm.net.mode === 'nat'}
                  NAT{#if vm.net.forwards?.length} · {vm.net.forwards.length} forward(s){/if}
                {:else}
                  LAN on {vm.net.nic}
                {/if}
              </td>
              <td>{vm.autostart ? 'yes' : 'no'}</td>
              <td>
                <div class="flex flex-wrap justify-end gap-1">
                  <Button variant="secondary" size="sm" on:click={() => (selected = vm)}>Open</Button>
                  <Button variant="secondary" size="sm" on:click={() => openEdit(vm)}>Edit</Button>
                  <Button
                    variant="danger"
                    size="sm"
                    loading={removing === vm.name}
                    disabled={!!removing}
                    on:click={() => removeVm(vm)}
                  >
                    Delete
                  </Button>
                </div>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </DataTable>

    {#if capabilities}
      <p class="text-xs text-gray-500 mt-2">
        Node: {capabilities.cpuModel || 'unknown CPU'} · KVM {capabilities.kvm ? 'yes' : 'no'}
        {capabilities.nested ? ' (nested)' : ''} ·
        Secure Boot {capabilities.secureBoot ? 'available' : 'unavailable'} ·
        VNC range 5901+{#if capabilities.nics} · NICs {capabilities.nics.join(', ')}{/if}
      </p>
    {/if}
  </AsyncState>
</div>

<VmWizard open={wizardOpen} vm={editing} pools={pools} images={wizardImages}
  secureBootAvailable={!!capabilities?.secureBoot}
  on:poolchange={(event) => loadWizardImages(event.detail)}
  close={() => (wizardOpen = false)} submit={createOrUpdate} />

{#if selected}
  <VmDetail vm={selected} close={() => (selected = null)} action={action}
    toggleAutostart={toggleAutostart} snapshot={snapshot} restore={restore} />
{/if}
