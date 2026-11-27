<script lang="ts">
  import Button from '../Button.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import type { Updates, VersionInfo } from './system';
  import { fmtTime } from '$lib/format';

  export let updates: Updates | null = null;
  export let version: VersionInfo;
  export let checking = false;
  export let notesUrl = '';
  export let onCheck: () => void = () => {};
</script>

<section class="card mb-6" aria-labelledby="system-updates">
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div>
      <div class="flex flex-wrap items-center gap-2">
        <h2 id="system-updates" class="text-lg font-bold">Updates</h2>
        {#if updates?.available}
          <StatusBadge tone="info" label="update available" />
        {/if}
      </div>
      <p class="mt-1 text-sm text-gray-400">
        {#if updates?.checkedAt}
          Checked {fmtTime(updates.checkedAt)}
        {:else}
          No update check has run yet.
        {/if}
      </p>
    </div>
    <Button
      variant="primary"
      loading={checking}
      disabled={checking || !version.updatesEnabled}
      on:click={onCheck}
    >
      {checking ? 'Checking…' : 'Check now'}
    </Button>
  </div>

  {#if updates?.error}
    <div class="callout callout-bad mt-3" role="alert">
      <p class="text-sm">{updates.error}</p>
    </div>
  {/if}

  {#if updates?.available}
    <div class="mt-4 rounded-lg border border-naslos-border bg-naslos-dark p-4">
      <p class="font-medium">
        {#if updates.available.version === version?.naslosVersion}
          System upgrade for Naslos {updates.available.version}
        {:else}
          Naslos {updates.available.version} is available
        {/if}
      </p>
      <p class="mt-1 text-sm text-gray-400">
        Talos {updates.available.talosVersion} · Kubernetes
        {updates.available.kubernetesVersion}
      </p>
      {#if notesUrl}
        <a
          class="mt-2 inline-block text-sm text-naslos-accent underline"
          href={notesUrl}
          target="_blank"
          rel="noopener noreferrer"
        >Release notes</a>
      {/if}
    </div>
  {:else if updates?.blocked}
    <p class="mt-3 text-sm text-gray-400">{updates.blocked}</p>
  {/if}
</section>
