<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from './Button.svelte';
  import FormField from './FormField.svelte';
  import Modal from './Modal.svelte';

  export let source: any | null = null;

  let name = source?.name || '';
  let displayName = source?.displayName || '';
  let url = source?.url || '';
  let auth = source?.auth || 'public';
  let credentialsSecret = source?.credentialsSecret || '';
  let saving = false;
  let error = '';

  const dispatch = createEventDispatcher();

  async function save() {
    saving = true;
    error = '';
    try {
      const body = { name, displayName, url, auth, credentialsSecret };
      const res = await fetch('/api/sources', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Save failed');
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }
</script>

<Modal title="Add Chart Repository" onClose={() => dispatch('close')}>
  <form id="source-form" on:submit|preventDefault={save} class="space-y-4">
    <FormField label="Name" id="source-name" required hint="Lowercase DNS-1123 label.">
      <input id="source-name" class="input w-full" bind:value={name} placeholder="mine" autocomplete="off" />
    </FormField>
    <FormField label="Git URL" id="source-url" required>
      <input
        id="source-url"
        class="input w-full"
        bind:value={url}
        placeholder="https://github.com/you/charts.git"
        autocomplete="off"
      />
    </FormField>
    <FormField label="Authentication" id="source-auth">
      <select id="source-auth" class="input w-full" bind:value={auth}>
        <option value="public">Public</option>
        <option value="token">HTTPS token</option>
        <option value="ssh">SSH deploy key</option>
      </select>
    </FormField>
    {#if auth !== 'public'}
      <FormField
        label="Credentials Secret"
        id="source-secret"
        hint="Secret in the release namespace holding {auth === 'token' ? 'a `token` key' : 'an `ssh-private-key` key'}."
      >
        <input
          id="source-secret"
          class="input w-full"
          bind:value={credentialsSecret}
          placeholder="naslos-charts-creds"
          autocomplete="off"
        />
      </FormField>
    {/if}
    {#if error}
      <p role="alert" class="callout callout-bad mb-0">{error}</p>
    {/if}
  </form>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button type="submit" form="source-form" loading={saving}>
      {saving ? 'Saving...' : 'Add Source'}
    </Button>
  </svelte:fragment>
</Modal>
