<script lang="ts">
  // Create/edit wizard (FR-VMT). Everything is validated server-side too;
  // this form just keeps the payload shapes honest.
  import { createEventDispatcher } from 'svelte';
  import Button from './Button.svelte';
  import FormField from './FormField.svelte';
  import Modal from './Modal.svelte';
  import type {
    VmForm,
    VmImage,
    VmPayload,
    VmView
  } from '$lib/vmTypes';

  export let open: boolean;
  export let vm: VmView | null; // null = create, object = edit
  export let pools: string[] = [];
  export let images: VmImage[] = []; // library entries of the selected pool
  // The supervisor image ships the OVMF Secure Boot pair (capabilities).
  export let secureBootAvailable = false;

  export let close: () => void = () => {};
  export let submit: (payload: VmPayload, edited: boolean) => Promise<string | null>;

  const dispatch = createEventDispatcher<{ poolchange: string }>();

  let busy = false;
  let error = '';
  let form: VmForm = blank();

  // Normalize the server view into the form shape. The server omits an empty
  // forwards list and has no cloud-init "enabled" flag, so a raw clone made the
  // wizard crash on VMs without cloud-init and silently drop it on save.
  function fromVm(record: VmView): VmForm {
    return {
      name: record.name,
      pool: record.pool,
      cpu: record.cpu,
      memMiB: record.memMiB,
      autostart: !!record.autostart,
      firmware: record.firmware,
      secureBoot: !!record.secureBoot,
      desired: record.desired ?? 'stopped',
      disk: {
        sizeGB: record.disk.sizeGB,
        format: record.disk.format,
        bus: record.disk.bus,
        sourceImage: record.disk.sourceImage ?? null
      },
      net: {
        mode: record.net?.mode ?? 'nat',
        forwards: (record.net?.forwards ?? []).map((fwd) => ({
          host: fwd.host,
          dst: fwd.dst
        }))
      },
      cloudInit: {
        enabled: !!record.cloudInit,
        hostname: record.cloudInit?.hostname ?? '',
        user: record.cloudInit?.user || 'admin',
        sshKeys: (record.cloudInit?.sshKeys ?? []).join('\n')
      },
      iso: record.iso ?? null,
      bootMode: record.disk?.sourceImage ? 'image' : 'iso'
    };
  }

  $: if (open) {
    error = '';
    form = vm ? fromVm(vm) : blank();
  }

  function blank(): VmForm {
    return {
      name: '',
      pool: pools[0] ?? '',
      cpu: 2,
      memMiB: 2048,
      autostart: false,
      firmware: 'uefi',
      secureBoot: false,
      desired: 'stopped',
      disk: { sizeGB: 40, format: 'qcow2', bus: 'virtio', sourceImage: null },
      net: { mode: 'nat', forwards: [] },
      cloudInit: { enabled: true, hostname: '', user: 'admin', sshKeys: '' },
      iso: null,
      bootMode: 'iso'
    };
  }

  $: isEdit = vm !== null;
  $: isos = images.filter((i) => /\.(iso)$/i.test(i.name));
  $: diskImages = images.filter((i) => !/\.(iso)$/i.test(i.name));

  function selectPool(event: Event) {
    form.pool = (event.currentTarget as HTMLSelectElement).value;
    dispatch('poolchange', form.pool);
  }

  function addForward() {
    form.net.forwards = [...form.net.forwards, { host: 5900, dst: 80 }];
  }
  function removeForward(index: number) {
    form.net.forwards = form.net.forwards.filter((_, i) => i !== index);
  }

  async function onSave() {
    if (busy) return;
    busy = true;
    error = '';
    try {
      const payload: VmPayload = {
        name: form.name.trim(),
        pool: form.pool,
        cpu: Number(form.cpu),
        memMiB: Number(form.memMiB),
        autostart: !!form.autostart,
        firmware: form.firmware,
        // The server rejects Secure Boot on BIOS; never send that shape.
        secureBoot: form.firmware === 'uefi' && !!form.secureBoot,
        // Keep the persisted desired state in edit mode; a new VM starts
        // stopped (start it explicitly from the detail view).
        desired: isEdit ? (vm?.desired ?? 'stopped') : 'stopped',
        disk: {
          sizeGB: Number(form.disk.sizeGB),
          format: form.disk.format,
          bus: form.disk.bus,
          sourceImage: form.bootMode === 'image' ? form.disk.sourceImage : null
        },
        net: {
          mode: form.net.mode,
          forwards: form.net.forwards.map((f) => ({ host: Number(f.host), dst: Number(f.dst) }))
        },
        iso: form.bootMode === 'iso' ? form.iso : null
      };
      if (form.cloudInit.enabled) {
        const keys = String(form.cloudInit.sshKeys)
          .split('\n')
          .map((k) => k.trim())
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
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal
  {open}
  title={isEdit ? `Edit ${vm?.name}` : 'New virtual machine'}
  size="lg"
  onClose={close}
>
  <form id="vm-form" on:submit|preventDefault={onSave} class="space-y-4">
    {#if error}
      <p role="alert" class="callout callout-bad mb-0">{error}</p>
    {/if}

    <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
      <FormField label="Name" id="vm-name" required>
        <input
          id="vm-name"
          class="input w-full"
          bind:value={form.name}
          disabled={isEdit}
          placeholder="lowercase-dns-name"
          autocomplete="off"
        />
      </FormField>
      <FormField label="Pool" id="vm-pool" required>
        <select id="vm-pool" class="input w-full" value={form.pool} on:change={selectPool} disabled={isEdit}>
          {#each pools as p}
            <option value={p}>{p}</option>
          {/each}
        </select>
      </FormField>
      <FormField label="vCPUs" id="vm-cpu">
        <input id="vm-cpu" type="number" min="1" max="64" class="input w-full" bind:value={form.cpu} />
      </FormField>
      <FormField label="Memory (MiB)" id="vm-mem">
        <input id="vm-mem" type="number" min="16" step="16" class="input w-full" bind:value={form.memMiB} />
      </FormField>
    </div>

    <h3 class="text-sm font-semibold text-gray-300">Disk</h3>
    <div class="grid grid-cols-1 gap-3 md:grid-cols-3">
      <FormField label="Size (GB)" id="vm-size">
        <input id="vm-size" type="number" min="1" max="16384" class="input w-full" bind:value={form.disk.sizeGB} />
      </FormField>
      <FormField label="Format" id="vm-format">
        <select id="vm-format" class="input w-full" bind:value={form.disk.format}>
          <option value="qcow2">qcow2 (sparse)</option>
          <option value="raw">raw</option>
        </select>
      </FormField>
      <FormField label="Bus" id="vm-bus">
        <select id="vm-bus" class="input w-full" bind:value={form.disk.bus}>
          <option value="virtio">virtio (fast)</option>
          <option value="sata">sata (Windows without drivers)</option>
        </select>
      </FormField>
    </div>

    <h3 class="text-sm font-semibold text-gray-300">Boot media</h3>
    <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
      <FormField label="Install ISO or seed image" id="vm-boot">
        <select id="vm-boot" class="input w-full" bind:value={form.bootMode}>
          <option value="iso">Attach an ISO from the library</option>
          <option value="image">Seed the disk from a cloud image</option>
        </select>
      </FormField>
      {#if form.bootMode === 'iso'}
        <FormField label="ISO" id="vm-iso">
          <select id="vm-iso" class="input w-full" bind:value={form.iso}>
            <option value={null}>None</option>
            {#each isos as img}
              <option value={`isos/${img.name}`}>{img.name}</option>
            {/each}
          </select>
        </FormField>
      {:else}
        <FormField label="Cloud image" id="vm-img">
          <select id="vm-img" class="input w-full" bind:value={form.disk.sourceImage}>
            <option value={null}>None (blank disk)</option>
            {#each diskImages as img}
              <option value={`isos/${img.name}`}>{img.name}</option>
            {/each}
          </select>
        </FormField>
      {/if}
    </div>
    <p class="text-xs text-gray-500">
      Import ISOs and cloud images in the Images section below first. A cloud image is copied to the
      VM's disk and resized to the declared size on first start.
    </p>

    <h3 class="text-sm font-semibold text-gray-300">Networking</h3>
    <p class="text-xs text-gray-500">
      NAT only: the guest shares the node's network and is reachable through these port forwards.
    </p>
    <div>
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span class="label mb-0">Port forwards (node port → guest port)</span>
        <Button variant="secondary" size="sm" on:click={addForward}>Add forward</Button>
      </div>
      {#each form.net.forwards as fwd, index}
        <div class="mt-2 flex flex-wrap items-center gap-2">
          <input
            type="number"
            min="1"
            max="65535"
            class="input w-32"
            aria-label={`Node port for forward ${index + 1}`}
            bind:value={fwd.host}
          />
          <span class="text-gray-400">→</span>
          <input
            type="number"
            min="1"
            max="65535"
            class="input w-32"
            aria-label={`Guest port for forward ${index + 1}`}
            bind:value={fwd.dst}
          />
          <Button variant="secondary" size="sm" on:click={() => removeForward(index)}>Remove</Button>
        </div>
      {/each}
    </div>

    <h3 class="text-sm font-semibold text-gray-300">Firmware &amp; autostart</h3>
    <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
      <div>
        <FormField label="Firmware" id="vm-fw">
          <select id="vm-fw" class="input w-full" bind:value={form.firmware}>
            <option value="uefi">UEFI (OVMF)</option>
            <option value="bios">BIOS (SeaBIOS)</option>
          </select>
        </FormField>
        <label class="mt-2 flex items-center gap-2 text-sm text-gray-300">
          <input type="checkbox" class="w-4 h-4 rounded" bind:checked={form.secureBoot}
            disabled={form.firmware !== 'uefi' || !secureBootAvailable} />
          Secure Boot (UEFI only)
        </label>
        <p class="text-xs text-gray-500 mt-1">
          {#if form.firmware !== 'uefi'}
            Secure Boot requires UEFI firmware.
          {:else if !secureBootAvailable}
            The supervisor image has no Secure Boot firmware; non-Secure-Boot UEFI is used.
          {:else}
            Boots the guest with the OVMF Secure Boot firmware and Microsoft-enrolled keys.
            Signed boot loaders only (Windows, or a Linux shim).
          {/if}
        </p>
      </div>
      <div class="flex items-start md:items-end">
        <label class="flex items-center gap-2 text-sm text-gray-300">
          <input type="checkbox" class="w-4 h-4 rounded" bind:checked={form.autostart} />
          Start with the platform (autostart)
        </label>
      </div>
    </div>

    <h3 class="text-sm font-semibold text-gray-300">cloud-init (first boot)</h3>
    <label class="mb-2 flex items-center gap-2 text-sm text-gray-300">
      <input type="checkbox" class="w-4 h-4 rounded" bind:checked={form.cloudInit.enabled} />
      Configure the guest on first boot (NoCloud seed)
    </label>
    {#if form.cloudInit.enabled}
      <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
        <FormField label="Hostname" id="vm-ci-host" hint="Defaults to the VM name.">
          <input id="vm-ci-host" class="input w-full" bind:value={form.cloudInit.hostname} placeholder="vm-name" />
        </FormField>
        <FormField label="User" id="vm-ci-user">
          <input id="vm-ci-user" class="input w-full" bind:value={form.cloudInit.user} />
        </FormField>
      </div>
      <FormField
        label="SSH public keys (one per line)"
        id="vm-ci-keys"
        hint="Password login is disabled in the seed; only these keys open the door."
      >
        <textarea
          id="vm-ci-keys"
          rows="3"
          class="input w-full font-mono text-xs"
          bind:value={form.cloudInit.sshKeys}
          placeholder="ssh-ed25519 AAAA…"
        ></textarea>
      </FormField>
    {/if}
  </form>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={close}>Cancel</Button>
    <Button type="submit" form="vm-form" loading={busy} disabled={!form.name.trim() || !form.pool}>
      {busy ? 'Saving…' : isEdit ? 'Save changes' : 'Create VM'}
    </Button>
  </svelte:fragment>
</Modal>
