<script lang="ts">
  // Create/edit wizard (FR-VMT). Everything is validated server-side too;
  // this form just keeps the payload shapes honest.
  import { modal } from '$lib/actions/modal';

  export let open: boolean;
  export let vm: any | null; // null = create, object = edit
  export let pools: string[] = [];
  export let capabilities: any = null;
  export let images: any[] = []; // library entries of the selected pool

  export let close: () => void = () => {};
  export let submit: (payload: any, edited: boolean) => Promise<string | null>;

  let busy = false;
  let error = '';
  let form: any = blank();

  $: if (open) {
    error = '';
    form = vm ? structuredClone(vm) : blank();
  }

  function blank() {
    return {
      name: '',
      pool: pools[0] ?? '',
      cpu: 2,
      memMiB: 2048,
      autostart: false,
      firmware: 'uefi',
      desired: 'stopped',
      disk: { sizeGB: 40, format: 'qcow2', bus: 'virtio', sourceImage: null },
      net: { mode: 'nat', forwards: [], nic: null },
      cloudInit: { enabled: true, hostname: '', user: 'admin', sshKeys: '' },
      iso: null,
      bootMode: 'iso'
    };
  }

  $: if (vm && open) {
    // Edit mode: derive the wizard's bootMode from the record.
    form.bootMode = vm.disk?.sourceImage ? 'image' : 'iso';
  }

  $: isEdit = vm !== null;
  $: lanSupported = capabilities?.macvtap !== false;
  $: isos = images.filter((i: any) => /\.(iso)$/i.test(i.name));
  $: diskImages = images.filter((i: any) => !/\.(iso)$/i.test(i.name));

  function addForward() {
    form.net.forwards = [...form.net.forwards, { host: 5900, dst: 80 }];
  }
  function removeForward(index: number) {
    form.net.forwards = form.net.forwards.filter((_: any, i: number) => i !== index);
  }

  async function onSave() {
    if (busy) return;
    busy = true;
    error = '';
    try {
      const payload: any = {
        name: form.name.trim(),
        pool: form.pool,
        cpu: Number(form.cpu),
        memMiB: Number(form.memMiB),
        autostart: !!form.autostart,
        firmware: form.firmware,
        // Keep the persisted desired state in edit mode; a new VM starts
        // stopped (start it explicitly from the detail view).
        desired: isEdit ? (vm.desired ?? 'stopped') : 'stopped',
        disk: {
          sizeGB: Number(form.disk.sizeGB),
          format: form.disk.format,
          bus: form.disk.bus,
          sourceImage: form.bootMode === 'image' ? form.disk.sourceImage : null
        },
        net: {
          mode: form.net.mode,
          forwards: form.net.mode === 'nat'
            ? form.net.forwards.map((f: any) => ({ host: Number(f.host), dst: Number(f.dst) }))
            : [],
          nic: form.net.mode === 'lan' ? form.net.nic : null
        },
        iso: form.bootMode === 'iso' ? form.iso : null
      };
      if (form.cloudInit.enabled) {
        const keys = String(form.cloudInit.sshKeys)
          .split('\n')
          .map((k: string) => k.trim())
          .filter(Boolean);
        payload.cloudInit = {
          hostname: form.cloudInit.hostname.trim() || form.name.trim(),
          user: form.cloudInit.user.trim(),
          sshKeys: keys
        };
      }
      const err = await submit(payload, isEdit);
      if (err) {
        error = err;
      } else {
        close();
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      busy = false;
    }
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="fixed inset-0 bg-black/60 z-40 flex items-start justify-center overflow-y-auto py-8" use:modal={{ onClose: close }} on:click|self={close}>
    <div class="card w-full max-w-2xl">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-xl font-bold">{isEdit ? `Edit ${vm.name}` : 'New virtual machine'}</h2>
        <button class="btn btn-secondary" on:click={close}>Close</button>
      </div>

      {#if error}
        <div role="alert" class="card mb-4 border-red-700 bg-red-900/20 text-red-300 text-sm">{error}</div>
      {/if}

      <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
        <div>
          <label class="label" for="vm-name">Name *</label>
          <input id="vm-name" class="input w-full" bind:value={form.name} disabled={isEdit}
            placeholder="lowercase-dns-name" />
        </div>
        <div>
          <label class="label" for="vm-pool">Pool *</label>
          <select id="vm-pool" class="input w-full" bind:value={form.pool} disabled={isEdit}>
            {#each pools as p}
              <option value={p}>{p}</option>
            {/each}
          </select>
        </div>
        <div>
          <label class="label" for="vm-cpu">vCPUs</label>
          <input id="vm-cpu" type="number" min="1" max="64" class="input w-full" bind:value={form.cpu} />
        </div>
        <div>
          <label class="label" for="vm-mem">Memory (MiB)</label>
          <input id="vm-mem" type="number" min="16" step="16" class="input w-full" bind:value={form.memMiB} />
        </div>
      </div>

      <h3 class="text-sm font-semibold text-gray-300 mt-5 mb-2">Disk</h3>
      <div class="grid grid-cols-1 md:grid-cols-3 gap-3">
        <div>
          <label class="label" for="vm-size">Size (GB)</label>
          <input id="vm-size" type="number" min="1" max="16384" class="input w-full" bind:value={form.disk.sizeGB} />
        </div>
        <div>
          <label class="label" for="vm-format">Format</label>
          <select id="vm-format" class="input w-full" bind:value={form.disk.format}>
            <option value="qcow2">qcow2 (sparse)</option>
            <option value="raw">raw</option>
          </select>
        </div>
        <div>
          <label class="label" for="vm-bus">Bus</label>
          <select id="vm-bus" class="input w-full" bind:value={form.disk.bus}>
            <option value="virtio">virtio (fast)</option>
            <option value="sata">sata (Windows without drivers)</option>
          </select>
        </div>
      </div>

      <h3 class="text-sm font-semibold text-gray-300 mt-5 mb-2">Boot media</h3>
      <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
        <div>
          <label class="label" for="vm-boot">Install ISO or seed image</label>
          <select id="vm-boot" class="input w-full" bind:value={form.bootMode}>
            <option value="iso">Attach an ISO from the library</option>
            <option value="image">Seed the disk from a cloud image</option>
          </select>
        </div>
        <div>
          {#if form.bootMode === 'iso'}
            <label class="label" for="vm-iso">ISO</label>
            <select id="vm-iso" class="input w-full" bind:value={form.iso}>
              <option value={null}>None</option>
              {#each isos as img}
                <option value={`isos/${img.name}`}>{img.name}</option>
              {/each}
            </select>
          {:else}
            <label class="label" for="vm-img">Cloud image</label>
            <select id="vm-img" class="input w-full" bind:value={form.disk.sourceImage}>
              <option value={null}>None (blank disk)</option>
              {#each diskImages as img}
                <option value={`isos/${img.name}`}>{img.name}</option>
              {/each}
            </select>
          {/if}
        </div>
      </div>
      <p class="text-xs text-gray-500 mt-1">
        Import ISOs and cloud images in the Images section below first. A cloud image is copied to the
        VM's disk and resized to the declared size on first start.
      </p>

      <h3 class="text-sm font-semibold text-gray-300 mt-5 mb-2">Networking</h3>
      <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
        <div>
          <label class="label" for="vm-net">Mode</label>
          <select id="vm-net" class="input w-full" bind:value={form.net.mode}>
            <option value="nat">NAT (port forwards)</option>
            <option value="lan" disabled={!lanSupported}>LAN (own DHCP address{lanSupported ? '' : ' — macvtap unavailable'})</option>
          </select>
        </div>
        {#if form.net.mode === 'lan'}
          <div>
            <label class="label" for="vm-nic">Physical NIC</label>
            <select id="vm-nic" class="input w-full" bind:value={form.net.nic}>
              <option value={null}>Choose…</option>
              {#each (capabilities?.nics ?? []) as nic}
                <option value={nic}>{nic}</option>
              {/each}
            </select>
            <p class="text-xs text-yellow-500/80 mt-1">
              The NAS host itself cannot talk to a LAN-mode guest (macvtap); use another device on the network.
            </p>
          </div>
        {/if}
      </div>
      {#if form.net.mode === 'nat'}
        <div class="mt-3">
          <div class="flex items-center justify-between">
            <span class="label">Port forwards (node port → guest port)</span>
            <button class="btn btn-secondary" on:click={addForward}>Add forward</button>
          </div>
          {#each form.net.forwards as fwd, index}
            <div class="flex items-center gap-2 mt-2">
              <input type="number" min="1" max="65535" class="input w-32" bind:value={fwd.host} />
              <span class="text-gray-400">→</span>
              <input type="number" min="1" max="65535" class="input w-32" bind:value={fwd.dst} />
              <button class="btn btn-secondary" on:click={() => removeForward(index)}>Remove</button>
            </div>
          {/each}
        </div>
      {/if}

      <h3 class="text-sm font-semibold text-gray-300 mt-5 mb-2">Firmware &amp; autostart</h3>
      <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
        <div>
          <label class="label" for="vm-fw">Firmware</label>
          <select id="vm-fw" class="input w-full" bind:value={form.firmware}>
            <option value="uefi">UEFI (OVMF)</option>
            <option value="bios">BIOS (SeaBIOS)</option>
          </select>
        </div>
        <div class="flex items-end">
          <label class="flex items-center gap-2 text-sm text-gray-300">
            <input type="checkbox" class="w-4 h-4 rounded" bind:checked={form.autostart} />
            Start with the platform (autostart)
          </label>
        </div>
      </div>

      <h3 class="text-sm font-semibold text-gray-300 mt-5 mb-2">cloud-init (first boot)</h3>
      <label class="flex items-center gap-2 text-sm text-gray-300 mb-2">
        <input type="checkbox" class="w-4 h-4 rounded" bind:checked={form.cloudInit.enabled} />
        Configure the guest on first boot (NoCloud seed)
      </label>
      {#if form.cloudInit.enabled}
        <div class="grid grid-cols-1 md:grid-cols-2 gap-3">
          <div>
            <label class="label" for="vm-ci-host">Hostname</label>
            <input id="vm-ci-host" class="input w-full" bind:value={form.cloudInit.hostname}
              placeholder="defaults to the VM name" />
          </div>
          <div>
            <label class="label" for="vm-ci-user">User</label>
            <input id="vm-ci-user" class="input w-full" bind:value={form.cloudInit.user} />
          </div>
        </div>
        <div class="mt-3">
          <label class="label" for="vm-ci-keys">SSH public keys (one per line)</label>
          <textarea id="vm-ci-keys" rows="3" class="input w-full font-mono text-xs"
            bind:value={form.cloudInit.sshKeys} placeholder="ssh-ed25519 AAAA…"></textarea>
          <p class="text-xs text-gray-500 mt-1">
            Password login is disabled in the seed; only these keys open the door.
          </p>
        </div>
      {/if}

      <div class="flex justify-end gap-2 mt-6">
        <button class="btn btn-secondary" on:click={close}>Cancel</button>
        <button class="btn btn-primary" on:click={onSave} disabled={busy}>
          {busy ? 'Saving…' : isEdit ? 'Save changes' : 'Create VM'}
        </button>
      </div>
    </div>
  </div>
{/if}
