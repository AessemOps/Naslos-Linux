<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import AppIcon from './AppIcon.svelte';
  import Button from './Button.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { formatUsage } from '$lib/format';
  import { statusTone } from '$lib/appStatus';
  import type { AppView, AppUsage } from '$lib/appTypes';

  export let app: AppView;
  export let usage: AppUsage | undefined = undefined;
  export let jobRunning = false;

  const dispatch = createEventDispatcher();

  $: displayName = app.displayName || app.name;
</script>

<div class="flex flex-wrap items-start gap-3 px-4 py-2.5">
  <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-2xl" imgClass="h-7 w-7" />

  <div class="flex-1 min-w-0">
    <div class="flex flex-wrap items-center gap-2">
      <h3 class="font-semibold truncate">
        <button type="button" class="hover:underline text-left" on:click={() => dispatch('open')}>
          {displayName}
        </button>
      </h3>
      <StatusBadge state={app.status} tone={statusTone(app.status)} />
      {#if app.orphaned}
        <span class="badge badge-warn" title="Found in the cluster without a catalog entry">orphaned</span>
      {/if}
      {#if jobRunning}
        <span class="badge badge-warn" title="A lifecycle job is running for this app">job running</span>
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
    {#if app.updateAvailable || app.imageUpdateAvailable}
      <Button
        variant="secondary"
        size="sm"
        disabled={jobRunning}
        aria-label={app.updateAvailable
          ? `→ v${app.latestVersion}: update ${displayName} to version ${app.latestVersion}`
          : 'new image: update to the newly released image'}
        on:click={() => dispatch('update')}
      >
        {app.updateAvailable ? `→ v${app.latestVersion}` : 'new image'}
      </Button>
    {/if}
    <Button
      variant="ghost"
      size="icon"
      aria-label={`Open ${displayName} details`}
      on:click={() => dispatch('open')}
    >
      ›
    </Button>
  </div>
</div>
