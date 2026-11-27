<script lang="ts">
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';

  export let poolName = '';
  export let onClose: () => void;
  export let onCreated: () => void;

  let name = '';
  let size = '';
  let blocksize = '';
  let compression = 'lz4';
  let saving = false;
  let error = '';

  async function create() {
    saving = true;
    error = '';
    try {
      const options: Record<string, string> = {};
      // The zvol property surface is deliberately narrow (FR-ZVOL-02): only
      // volblocksize and compression are accepted here.
      if (blocksize) options.volblocksize = blocksize;
      if (compression) options.compression = compression;
      await api('/api/zvols', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          pool: poolName,
          name: name.trim(),
          size: size.trim(),
          options
        })
      });
      onCreated();
    } catch (e) {
      error = e instanceof ApiError ? e.message : 'Could not create the zvol. Check the API connection.';
    } finally {
      saving = false;
    }
  }
</script>

<Modal title="New zvol" {onClose}>
  <p class="text-sm text-gray-400">
    A zvol inside <span class="text-naslos-link">{poolName}</span> is a raw block device. It has no
    filesystem until a consumer formats it, and it cannot be shared over SMB/NFS.
  </p>

  <form id="new-zvol-form" on:submit|preventDefault={create} class="space-y-4">
    <FormField label="Name" id="zvol-name" required hint="Nested names work: vms/disk1.">
      <input
        id="zvol-name"
        type="text"
        bind:value={name}
        placeholder="e.g. vm-disk1"
        class="input w-full"
        required
      />
    </FormField>

    <FormField
      label="Size"
      id="zvol-size"
      required
      hint="A ZFS size such as 512M, 10G or 1.5T."
    >
      <input
        id="zvol-size"
        type="text"
        bind:value={size}
        placeholder="e.g. 10G"
        class="input w-full"
        required
      />
    </FormField>

    <FormField label="Block size" id="zvol-blocksize">
      <select id="zvol-blocksize" bind:value={blocksize} class="input w-full">
        <option value="">Default (leave to ZFS)</option>
        <option value="4096">4 KiB</option>
        <option value="8192">8 KiB</option>
        <option value="16384">16 KiB</option>
        <option value="32768">32 KiB</option>
        <option value="65536">64 KiB</option>
        <option value="131072">128 KiB</option>
        <option value="1048576">1 MiB</option>
      </select>
    </FormField>

    <FormField label="Compression" id="zvol-compression">
      <select id="zvol-compression" bind:value={compression} class="input w-full">
        <option value="lz4">lz4 (fast, good default)</option>
        <option value="zstd">zstd (better ratio, more CPU)</option>
        <option value="off">off</option>
        <option value="gzip">gzip</option>
      </select>
    </FormField>

    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <div slot="footer">
    <Button variant="secondary" on:click={onClose}>Cancel</Button>
    <Button
      type="submit"
      form="new-zvol-form"
      loading={saving}
      disabled={!name.trim() || !size.trim()}
    >
      Create zvol
    </Button>
  </div>
</Modal>
