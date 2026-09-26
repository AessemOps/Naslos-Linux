<script lang="ts">
  import { createEventDispatcher } from 'svelte';

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

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">Add Chart Repository</h2>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>×</button>
    </div>
    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label" for="source-name">Name</label>
        <input id="source-name" class="input" bind:value={name} placeholder="mine" />
        <p class="text-xs text-gray-500 mt-1">Lowercase DNS-1123 label.</p>
      </div>
      <div>
        <label class="label" for="source-url">Git URL</label>
        <input id="source-url" class="input" bind:value={url} placeholder="https://github.com/you/charts.git" />
      </div>
      <div>
        <label class="label" for="source-auth">Authentication</label>
        <select id="source-auth" class="input" bind:value={auth}>
          <option value="public">Public</option>
          <option value="token">HTTPS token</option>
          <option value="ssh">SSH deploy key</option>
        </select>
      </div>
      {#if auth !== 'public'}
        <div>
          <label class="label" for="source-secret">Credentials Secret</label>
          <input id="source-secret" class="input" bind:value={credentialsSecret} placeholder="naslos-charts-creds" />
          <p class="text-xs text-gray-500 mt-1">
            Secret in the release namespace holding {auth === 'token' ? 'a `token` key' : 'an `ssh-private-key` key'}.
          </p>
        </div>
      {/if}
      {#if error}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving}>{saving ? 'Saving...' : 'Add Source'}</button>
    </div>
  </div>
</div>
