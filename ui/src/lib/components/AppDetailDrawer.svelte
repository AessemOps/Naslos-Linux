<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { modal } from '$lib/actions/modal';
  import { trackAppJob } from '$lib/stores/appJobs';
  import AppIcon from './AppIcon.svelte';
  import Sparkline from './Sparkline.svelte';
  import { formatBytes, formatBytesPerSec, formatMillicores } from '$lib/format';
  import { statusClass, statusLabel } from '$lib/appStatus';
  import type { AppView, AppUsage, AppUsageHistory } from '$lib/appTypes';

  export let app: AppView;
  export let usage: AppUsage | undefined = undefined;
  export let history: AppUsageHistory | undefined = undefined;
  export let jobRunning = false;

  const dispatch = createEventDispatcher();

  let uninstalling = false;
  let actionError = '';

  $: displayName = app.displayName || app.name;
  $: sourceSwitch =
    !!app.latestSource &&
    (app.latestSource !== (app.source ?? '') || app.latestChannel !== (app.channel ?? ''));
  $: exposure = app.exposure ?? { subdomain: '', tls: false, auth: false, localOnly: false, internet: true };

  function collecting(values: number[] | undefined): boolean {
    return !values || values.length < 2;
  }

  function fmtDate(value: string | null | undefined): string {
    if (!value) return 'unknown';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? 'unknown' : date.toLocaleString();
  }

  async function uninstallApp() {
    if (!confirm(`Are you sure you want to uninstall ${displayName}? This will remove all app data.`)) {
      return;
    }
    uninstalling = true;
    actionError = '';
    try {
      const res = await fetch(`/api/apps/${encodeURIComponent(app.name)}`, { method: 'DELETE' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      if (data.jobId) trackAppJob(data.jobId);
      dispatch('close');
    } catch (e: any) {
      actionError = e?.message || String(e);
    } finally {
      uninstalling = false;
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="fixed inset-0 z-50" use:modal={{ onClose: () => dispatch('close') }}>
  <div class="absolute inset-0 bg-black/60" on:click={() => dispatch('close')}></div>

  <aside
    class="absolute inset-y-0 right-0 w-full max-w-md bg-naslos-surface border-l border-naslos-border shadow-xl flex flex-col"
  >
    <div class="p-4 border-b border-naslos-border flex items-start gap-3">
      <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-3xl" imgClass="h-9 w-9" />
      <div class="flex-1 min-w-0">
        <h2 class="text-lg font-bold truncate">{displayName}</h2>
        <div class="flex flex-wrap items-center gap-2 mt-1">
          <span class={`text-xs px-2 py-0.5 rounded ${statusClass(app.status)}`}>{statusLabel(app.status)}</span>
          <span class="text-xs text-gray-500 font-mono truncate">{app.namespace}</span>
        </div>
      </div>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close details" on:click={() => dispatch('close')}>
        ×
      </button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-6">
      <!-- Version / update -->
      <section>
        <h3 class="text-xs uppercase tracking-wide text-gray-500 mb-2">Version</h3>
        <div class="flex items-center gap-3 flex-wrap">
          <p class="tabular-nums text-sm">
            v{app.installedVersion || 'unknown'}
            {#if app.latestVersion}
              <span class="text-gray-500">→</span>
              v{app.latestVersion}
            {/if}
          </p>
          {#if app.updateAvailable}
            <button
              type="button"
              class="text-xs px-2 py-1 rounded bg-amber-900/50 text-amber-300 hover:bg-amber-800/60"
              disabled={jobRunning}
              on:click={() => dispatch('update')}
            >
              Update
            </button>
          {:else if !app.latestVersion}
            <span class="text-xs text-gray-500">not in the catalog</span>
          {:else}
            <span class="text-xs text-gray-500">up to date</span>
          {/if}
        </div>
        {#if app.latestSource}
          <p class="text-xs text-gray-500 mt-1 font-mono">
            catalog {app.latestSource}/{app.latestChannel} · record {app.source || '?'}/{app.channel || '?'}
          </p>
        {/if}
        {#if sourceSwitch}
          <p class="text-xs text-amber-300 mt-2">
            Updating resolves the catalog winner; the app would move to
            {app.latestSource}/{app.latestChannel}.
          </p>
        {/if}
      </section>

      <!-- Live usage -->
      <section>
        <div class="flex items-center justify-between mb-2">
          <h3 class="text-xs uppercase tracking-wide text-gray-500">Usage</h3>
          <span class="text-xs text-gray-500">live · updates every 5s</span>
        </div>
        {#if !usage}
          <p class="text-xs text-gray-500">Usage is unavailable right now.</p>
        {:else}
          <div class="grid grid-cols-2 gap-3">
            <div class="rounded-lg border border-naslos-border p-3">
              <div class="text-xs text-gray-400">CPU</div>
              <div class="text-lg font-semibold tabular-nums">{formatMillicores(usage.cpuMillicores)}</div>
              {#if collecting(history?.cpu)}
                <p class="text-xs text-gray-500">collecting…</p>
              {:else}
                <Sparkline values={history?.cpu ?? []} />
              {/if}
            </div>
            <div class="rounded-lg border border-naslos-border p-3">
              <div class="text-xs text-gray-400">RAM</div>
              <div class="text-lg font-semibold tabular-nums">{formatBytes(usage.memoryBytes)}</div>
              {#if collecting(history?.memory)}
                <p class="text-xs text-gray-500">collecting…</p>
              {:else}
                <Sparkline values={history?.memory ?? []} />
              {/if}
            </div>
            <div class="rounded-lg border border-naslos-border p-3">
              <div class="text-xs text-gray-400">Network in</div>
              <div class="text-lg font-semibold tabular-nums">{formatBytesPerSec(usage.networkRxBytesPerSec)}</div>
              {#if collecting(history?.rx)}
                <p class="text-xs text-gray-500">collecting…</p>
              {:else}
                <Sparkline values={history?.rx ?? []} />
              {/if}
            </div>
            <div class="rounded-lg border border-naslos-border p-3">
              <div class="text-xs text-gray-400">Network out</div>
              <div class="text-lg font-semibold tabular-nums">{formatBytesPerSec(usage.networkTxBytesPerSec)}</div>
              {#if collecting(history?.tx)}
                <p class="text-xs text-gray-500">collecting…</p>
              {:else}
                <Sparkline values={history?.tx ?? []} />
              {/if}
            </div>
          </div>
          <p class="text-xs text-gray-500 mt-2 tabular-nums">{usage.pods} pods sampled</p>
        {/if}
      </section>

      <!-- Details -->
      <section class="space-y-2 text-sm">
        <h3 class="text-xs uppercase tracking-wide text-gray-500">Details</h3>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">URL</span>
          {#if app.url}
            <a class="text-naslos-accent hover:underline truncate" href={app.url} target="_blank" rel="noreferrer"
              >{app.url}</a
            >
          {:else}
            <span class="text-gray-400">{exposure.subdomain || 'cluster-internal only'}</span>
          {/if}
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">Chart</span>
          <span class="font-mono text-xs truncate">{app.chart || '—'}</span>
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">Created</span>
          <span class="text-xs">{fmtDate(app.createdAt)}</span>
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">Updated</span>
          <span class="text-xs">{fmtDate(app.updatedAt)}</span>
        </div>
        <div>
          <span class="text-gray-500">Datasets</span>
          {#if app.datasets?.length}
            <div class="flex flex-wrap gap-1 mt-1">
              {#each app.datasets as dataset (dataset)}
                <span class="text-xs px-2 py-0.5 rounded bg-naslos-dark border border-naslos-border font-mono"
                  >{dataset}</span
                >
              {/each}
            </div>
          {:else}
            <p class="text-gray-400 text-xs mt-1">none</p>
          {/if}
        </div>
        <div class="flex flex-wrap gap-2 pt-1 text-xs">
          <span class="px-2 py-0.5 rounded bg-naslos-dark border border-naslos-border"
            >TLS {exposure.tls ? 'on' : 'off'}</span
          >
          <span class="px-2 py-0.5 rounded bg-naslos-dark border border-naslos-border"
            >auth {exposure.auth ? 'on' : 'off'}</span
          >
          <span class="px-2 py-0.5 rounded bg-naslos-dark border border-naslos-border"
            >local only {exposure.localOnly ? 'on' : 'off'}</span
          >
          <span class="px-2 py-0.5 rounded bg-naslos-dark border border-naslos-border"
            >internet {exposure.internet ? 'on' : 'off'}</span
          >
        </div>
      </section>

      {#if jobRunning}
        <p class="text-xs text-yellow-300" role="status">A lifecycle job is running for this app.</p>
      {/if}
      {#if app.lastError}
        <p class="text-xs text-amber-400 break-words" role="alert">{app.lastError}</p>
      {/if}
      {#if actionError}
        <p class="text-xs text-red-300 break-words" role="alert">{actionError}</p>
      {/if}
    </div>

    <div class="p-4 border-t border-naslos-border flex flex-wrap gap-2">
      {#if app.url}
        <a class="btn btn-secondary" href={app.url} target="_blank" rel="noreferrer">Open</a>
      {:else}
        <button class="btn btn-secondary opacity-50 cursor-not-allowed" disabled title="No URL is configured"
          >Open</button
        >
      {/if}
      <button
        class="btn btn-secondary"
        disabled={app.orphaned || jobRunning}
        title={app.orphaned ? 'Orphaned releases cannot be reconfigured' : ''}
        on:click={() => dispatch('configure')}>Configure</button
      >
      <button class="btn btn-secondary" disabled={jobRunning} on:click={() => dispatch('exposure')}>Exposure</button>
      <button class="btn btn-danger ml-auto" disabled={jobRunning || uninstalling} on:click={uninstallApp}>
        {uninstalling ? 'Removing…' : 'Uninstall'}
      </button>
    </div>
  </aside>
</div>
