<script lang="ts">
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { zfsExactSize } from '$lib/storage';

  interface Zvol {
    name: string;
    volsize: string;
    volsizeBytes: number;
  }

  export let zvol: Zvol;
  export let onClose: () => void;
  export let onResized: () => void;

  // Prefill the exact size: the listing's `volsize` is rounded for display, so
  // resubmitting it can be unparseable (1.2M) or a silent shrink (1.0M for
  // 1048577 bytes). The API accepts a plain byte count or an exact unit.
  let size = zfsExactSize(zvol.volsizeBytes);
  let saving = false;
  let error = '';

  async function resize() {
    saving = true;
    error = '';
    try {
      await api(`/api/zvols?name=${encodeURIComponent(zvol.name)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ size: size.trim() })
      });
      onResized();
    } catch (e) {
      error = e instanceof ApiError ? e.message : 'Could not resize the zvol. Check the API connection.';
    } finally {
      saving = false;
    }
  }
</script>

<Modal title={`Resize ${zvol.name}`} {onClose}>
  <p class="text-sm text-gray-400">
    Current size: <strong class="font-mono">{zvol.volsize}</strong>
    {#if zvol.volsizeBytes > 0}
      (<code class="font-mono text-xs">{zvol.volsizeBytes}</code> bytes)
    {/if}. Resizing only grows a zvol: a shrink is refused because it can corrupt the filesystem
    inside it, and a resize is refused while the zvol is in use.
  </p>

  <form id="resize-zvol-form" on:submit|preventDefault={resize} class="space-y-4">
    <FormField label="New size" id="zvol-resize-size" required>
      <input
        id="zvol-resize-size"
        type="text"
        bind:value={size}
        placeholder="e.g. 20G"
        class="input w-full"
        required
      />
    </FormField>

    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <div slot="footer">
    <Button variant="secondary" on:click={onClose}>Cancel</Button>
    <Button type="submit" form="resize-zvol-form" loading={saving} disabled={!size.trim()}>
      Resize
    </Button>
  </div>
</Modal>
