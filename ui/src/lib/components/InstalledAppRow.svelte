<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import AppIcon from './AppIcon.svelte';
  import { formatUsage } from '$lib/format';
  import { statusClass, statusLabel } from '$lib/appStatus';
  import type { AppView, AppUsage } from '$lib/appTypes';

  export let app: AppView;
  export let usage: AppUsage | undefined = undefined;
  export let jobRunning = false;

  const dispatch = createEventDispatcher();

  $: displayName = app.displayName || app.name;
</script>

<div class="flex items-start gap-3 px-4 py-2.5">
  <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-2xl" imgClass="h-7 w-7" />

  <div class="flex-1 min-w-0">
    <div class="flex flex-wrap items-center gap-2">
      <h3 class="font-semibold truncate">
        <button type="button" class="hover:underline text-left" on:click={() => dispatch('open')}>
          {displayName}
        </button>
      </h3>
      <span class={`text-xs px-2 py-0.5 rounded ${statusClass(app.status)}`}>{statusLabel(app.status)}</span>
      {#if app.orphaned}
        <span
          class="text-xs px-2 py-0.5 rounded bg-amber-900/50 text-amber-300"
          title="Found in the cluster without a catalog entry">orphaned</span
        >
      {/if}
      {#if jobRunning}
        <span
          class="text-xs px-2 py-0.5 rounded bg-yellow-900/50 text-yellow-300"
          title="A lifecycle job is running for this app">job running</span
        >
      {/if}
    </div>

    <div class="mt-0.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-gray-500">
      <span class="tabular-nums">v{app.installedVersion || '?'}</span>
      <span class="tabular-nums text-gray-400">{formatUsage(usage)}</span>
    </div>

    {#if app.lastError}
      <p class="text-xs text-amber-400 mt-1 break-words">{app.lastError}</p>
    {/if}
  </div>

  <div class="flex items-center gap-2 shrink-0">
    {#if app.updateAvailable}
      <button
        type="button"
        class="text-xs px-2 py-1 rounded bg-amber-900/50 text-amber-300 hover:bg-amber-800/60 tabular-nums"
        aria-label={`Update ${displayName} to ${app.latestVersion}`}
        disabled={jobRunning}
        on:click={() => dispatch('update')}
      >
        → v{app.latestVersion}
      </button>
    {/if}
    <button
      type="button"
      class="text-gray-400 hover:text-white px-2 py-1 rounded"
      aria-label={`Open ${displayName} details`}
      on:click={() => dispatch('open')}
    >
      ›
    </button>
  </div>
</div>
