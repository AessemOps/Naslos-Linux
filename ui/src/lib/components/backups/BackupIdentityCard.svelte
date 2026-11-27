<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import type { Identity } from './types';

  export let identity: Identity | null = null;
  export let creating = false;
  export let copied = false;
  export let error = '';

  const dispatch = createEventDispatcher<{ create: { name: string }; copy: void }>();

  let identityName = '';
</script>

<div class="card mb-6">
  <h2 class="mb-2 text-lg font-bold">This instance</h2>
  {#if identity?.exists}
    <p class="text-sm text-gray-400">Name: <span class="text-gray-200">{identity.name}</span></p>
    <p class="text-sm text-gray-400">
      Fingerprint: <code class="select-all break-all text-naslos-link">{identity.fingerprint}</code>
    </p>
    <div class="mt-2 flex flex-wrap items-start gap-2">
      <code class="break-all rounded border border-naslos-border bg-naslos-dark px-2 py-1 text-xs text-gray-300 select-all">{identity.publicKey}</code>
      <Button size="sm" variant="secondary" on:click={() => dispatch('copy')}>
        {copied ? 'Copied' : 'Copy'}
      </Button>
    </div>
    <p class="callout callout-warn mt-3 text-xs">
      Back this identity file up somewhere your buddies cannot reach: losing it means losing the
      ability to read your backups.
    </p>
  {:else}
    <p class="mb-3 text-sm text-gray-400">
      No backup identity yet. Its public key is what a buddy authorizes.
    </p>
    <form
      class="flex flex-col gap-2 sm:flex-row sm:items-end"
      on:submit|preventDefault={() => dispatch('create', { name: identityName })}
    >
      <div class="sm:flex-1">
        <FormField label="Instance name" id="backup-identity-name">
          <input
            id="backup-identity-name"
            type="text"
            bind:value={identityName}
            placeholder="Instance name (optional)"
            class="input w-full"
          />
        </FormField>
      </div>
      <Button type="submit" loading={creating}>
        {creating ? 'Creating...' : 'Create identity'}
      </Button>
    </form>
    {#if error}
      <p role="alert" class="mt-2 text-sm text-red-300">{error}</p>
    {/if}
  {/if}
</div>
