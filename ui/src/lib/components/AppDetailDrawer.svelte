<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { modal } from '$lib/actions/modal';
  import { confirmAction } from '$lib/stores/confirm';
  import AppIcon from './AppIcon.svelte';
  import AppLogs from './AppLogs.svelte';
  import AppResourcesEditor from './AppResourcesEditor.svelte';
  import AppUsagePanel from './AppUsagePanel.svelte';
  import Button from './Button.svelte';
  import Icon from './Icon.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { statusTone } from '$lib/appStatus';
  import { trackAppJob } from '$lib/stores/appJobs';
  import { fmtTime } from '$lib/format';
  import type { AppView, AppUsage, AppUsageHistory } from '$lib/appTypes';

  export let app: AppView;
  export let usage: AppUsage | undefined = undefined;
  export let history: AppUsageHistory | undefined = undefined;
  export let jobRunning = false;

  const dispatch = createEventDispatcher();

  let uninstalling = false;
  let actionError = '';
  let dnsBusy = false;
  let dnsMessage = '';

  $: displayName = app.displayName || app.name;
  $: sourceSwitch =
    !!app.latestSource &&
    (app.latestSource !== (app.source ?? '') || app.latestChannel !== (app.channel ?? ''));
  $: exposure = app.exposure ?? { subdomain: '', tls: false, auth: false, localOnly: false, internet: true };

  async function retryDns() {
    dnsBusy = true;
    dnsMessage = '';
    try {
      const res = await fetch(`/api/apps/${encodeURIComponent(app.name)}/dns/reconcile`, {
        method: 'POST'
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      app = { ...app, ...data };
      dnsMessage = app.dns?.status === 'ok' ? 'DNS record is up to date.' : 'DNS reconcile ran.';
    } catch (e: any) {
      dnsMessage = e?.message || String(e);
    } finally {
      dnsBusy = false;
    }
  }

  async function uninstallApp() {
    const confirmed = await confirmAction({
      title: 'Uninstall app',
      message: `Uninstall ${displayName}? This removes the release and all of its app data, including its datasets.`,
      confirmLabel: 'Uninstall',
      tone: 'danger',
      typedConfirmation: app.name
    });
    if (!confirmed) return;
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
    class="absolute inset-y-0 right-0 flex w-full max-w-xl flex-col border-l border-naslos-border bg-naslos-surface shadow-xl"
  >
    <div class="flex items-start gap-3 border-b border-naslos-border p-4">
      <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-3xl" imgClass="h-9 w-9" />
      <div class="flex-1 min-w-0">
        <h2 class="text-lg font-bold truncate">{displayName}</h2>
        <div class="flex flex-wrap items-center gap-2 mt-1">
          <StatusBadge state={app.status} label={app.status} tone={statusTone(app.status)} />
          <span class="text-xs text-gray-500 font-mono truncate">{app.namespace}</span>
        </div>
      </div>
      <button
        class="icon-btn -mr-2 text-gray-400 transition-colors hover:bg-naslos-border hover:text-naslos-strong"
        aria-label="Close details"
        on:click={() => dispatch('close')}
      >
        <Icon name="x" />
      </button>
    </div>

    <div class="flex-1 space-y-6 overflow-y-auto p-6">
      <section>
        <h3 class="text-xs uppercase tracking-wide text-gray-500 mb-2">Version</h3>
        <div class="flex items-center gap-3 flex-wrap">
          <p class="tabular-nums text-sm">
            v{app.installedVersion || 'unknown'}
            {#if app.updateAvailable && app.latestVersion}
              <span class="text-gray-500">→</span>
              v{app.latestVersion}
            {/if}
          </p>
          {#if app.updateAvailable}
            <Button size="sm" variant="secondary" disabled={jobRunning} on:click={() => dispatch('update')}>
              Update
            </Button>
          {:else if app.imageUpdateAvailable}
            <Button size="sm" variant="secondary" disabled={jobRunning} on:click={() => dispatch('update')}>
              Update image
            </Button>
          {:else if !app.latestVersion}
            <span class="text-xs text-gray-500">not in the catalog</span>
          {:else}
            <span class="text-xs text-gray-500">up to date</span>
          {/if}
        </div>
        {#if app.imageUpdateAvailable && app.imageLatest}
          <p class="text-xs text-gray-500 mt-1 font-mono">
            image {app.imageCurrent || '?'} → {app.imageLatest}
          </p>
        {/if}
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

      <AppUsagePanel {usage} {history} />

      {#if jobRunning}
        <p class="text-xs text-yellow-300" role="status">A lifecycle job is running for this app.</p>
      {/if}

      <AppResourcesEditor {app} {jobRunning} />

      {#key app.name}
        <AppLogs {app} />
      {/key}

      <section class="space-y-2 text-sm">
        <h3 class="text-xs uppercase tracking-wide text-gray-500">Details</h3>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">URL</span>
          {#if app.url}
            <a class="text-naslos-link hover:underline truncate" href={app.url} target="_blank" rel="noreferrer"
              >{app.url}</a
            >
          {:else}
            <span class="text-gray-400">{exposure.subdomain || 'cluster-internal only'}</span>
          {/if}
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">DNS</span>
          <div class="text-right min-w-0">
            {#if app.dns}
              <p class="font-mono text-xs truncate">{app.dns.fqdn} → {app.dns.target}</p>
              <p class="text-xs" class:text-emerald-300={app.dns.status === 'ok'} class:text-red-300={app.dns.status !== 'ok'}>
                {app.dns.status}{#if app.dns.error}: {app.dns.error}{/if}
              </p>
              <Button variant="ghost" size="sm" loading={dnsBusy} on:click={retryDns}>
                {dnsBusy ? 'Retrying…' : 'Retry'}
              </Button>
            {:else if exposure.subdomain}
              <p class="text-xs text-gray-400">No app DNS record (enable it on the base domain)</p>
            {:else}
              <p class="text-xs text-gray-400">cluster-internal</p>
            {/if}
            {#if dnsMessage}<p class="text-xs text-gray-400 mt-1">{dnsMessage}</p>{/if}
          </div>
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">Chart</span>
          <span class="font-mono text-xs truncate">{app.chart || '—'}</span>
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">Created</span>
          <span class="text-xs">{fmtTime(app.createdAt)}</span>
        </div>
        <div class="flex justify-between gap-3">
          <span class="text-gray-500">Updated</span>
          <span class="text-xs">{fmtTime(app.updatedAt)}</span>
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
          <span class="badge badge-neutral">TLS {exposure.tls ? 'on' : 'off'}</span>
          <span class="badge badge-neutral">auth {exposure.auth ? 'on' : 'off'}</span>
          <span class="badge badge-neutral">local only {exposure.localOnly ? 'on' : 'off'}</span>
          <span class="badge badge-neutral">internet {exposure.internet ? 'on' : 'off'}</span>
        </div>
      </section>

      {#if app.lastError}
        <p class="text-xs text-amber-400 break-words" role="alert">{app.lastError}</p>
      {/if}
      {#if actionError}
        <p class="text-xs text-red-300 break-words" role="alert">{actionError}</p>
      {/if}
    </div>

    <div class="flex flex-wrap gap-2 border-t border-naslos-border p-4">
      {#if app.url}
        <Button variant="secondary" href={app.url} target="_blank" rel="noreferrer">Open</Button>
      {:else}
        <Button variant="secondary" disabled title="No URL is configured">Open</Button>
      {/if}
      <Button
        variant="secondary"
        disabled={app.orphaned || jobRunning}
        title={app.orphaned ? 'Orphaned releases cannot be reconfigured' : ''}
        on:click={() => dispatch('configure')}
      >
        Configure
      </Button>
      <Button variant="secondary" disabled={jobRunning} on:click={() => dispatch('exposure')}>Exposure</Button>
      <span class="sm:ml-auto">
        <Button variant="danger" disabled={jobRunning} loading={uninstalling} on:click={uninstallApp}>
          {uninstalling ? 'Removing…' : 'Uninstall'}
        </Button>
      </span>
    </div>
  </aside>
</div>
