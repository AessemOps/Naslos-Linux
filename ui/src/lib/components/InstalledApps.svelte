<script lang="ts">
  import { onMount } from 'svelte';
  import AppExposureModal from './AppExposureModal.svelte';
  import AppConfigureModal from './AppConfigureModal.svelte';
  import { appJobs, trackAppJob } from '$lib/stores/appJobs';

  interface AppView {
    name: string;
    namespace: string;
    chart: string;
    version: string;
    status: string;
    updatedAt: string;
    description: string;
    url: string;
    orphaned: boolean;
    lastError?: string;
    exposure: {
      subdomain: string;
      tls: boolean;
      auth: boolean;
      localOnly: boolean;
    };
  }

  let apps: AppView[] = [];
  let loading = true;
  let error = '';
  let editingExposure: string | null = null;
  let editingConfig: string | null = null;
  // Apps with a job currently running: their actions are disabled and a badge
  // marks them as in-flight.
  let runningJobApps = new Set<string>();
  let previousRunning = new Set<string>();

  async function loadApps() {
    loading = true;
    error = '';
    try {
      const res = await fetch('/api/apps');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      apps = Array.isArray(data) ? data : [];
    } catch (e) {
      error = 'Failed to load installed apps: ' + e;
      apps = [];
    } finally {
      loading = false;
    }
  }

  async function uninstallApp(name: string) {
    if (!confirm(`Are you sure you want to uninstall ${name}? This will remove all app data.`)) {
      return;
    }
    error = '';
    try {
      const res = await fetch(`/api/apps/${name}`, { method: 'DELETE' });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      // The uninstall runs as a background job; the list refetches when it ends.
      const data = await res.json();
      if (data.jobId) trackAppJob(data.jobId);
    } catch (e) {
      error = `Failed to uninstall ${name}: ` + e;
    }
  }

  function statusClass(status: string): string {
    switch (status) {
      case 'running': return 'bg-green-900/50 text-green-400';
      case 'failed': return 'bg-red-900/50 text-red-400';
      case 'pending': return 'bg-yellow-900/50 text-yellow-400';
      case 'missing': return 'bg-amber-900/50 text-amber-400';
      default: return 'bg-gray-900/50 text-gray-400';
    }
  }

  // Follow the shared job store: mark in-flight apps, and reload once a job for
  // a listed app finishes so its status/record reflects the outcome.
  const unsubscribeJobs = appJobs.subscribe((jobs) => {
    const running = new Set(jobs.filter((j) => j.state === 'running').map((j) => j.app));
    let finished = false;
    for (const name of previousRunning) {
      if (!running.has(name) && apps.some((a) => a.name === name)) finished = true;
    }
    previousRunning = running;
    runningJobApps = running;
    if (finished) loadApps();
  });

  onMount(() => {
    loadApps();
    return () => unsubscribeJobs();
  });
</script>

<div>
  {#if error}
    <div class="card border-red-700 bg-red-900/30 text-red-300 p-4 mb-4">{error}</div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading installed apps...</p>
  {:else if apps.length === 0 && !error}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">📦</div>
      <h2 class="text-xl font-bold mb-2">No Apps Installed</h2>
      <p class="text-gray-400 mb-4">Browse the catalog to install your first app.</p>
    </div>
  {:else}
    <div class="space-y-3">
      {#each apps as app (app.name)}
        <div class="card flex items-center gap-4">
          <div class="flex-1 min-w-0">
            <div class="flex items-center gap-3 mb-1">
              <h3 class="font-bold">{app.name}</h3>
              <span class={`text-xs px-2 py-0.5 rounded ${statusClass(app.status)}`}>{app.status}</span>
              {#if app.orphaned}
                <span class="text-xs px-2 py-0.5 rounded bg-amber-900/50 text-amber-300" title="Found in the cluster without a catalog entry">orphaned</span>
              {/if}
              {#if runningJobApps.has(app.name)}
                <span class="text-xs px-2 py-0.5 rounded bg-yellow-900/50 text-yellow-300" title="A lifecycle job is running for this app">job running</span>
              {/if}
            </div>
            {#if app.url}
              <a class="text-sm text-naslos-accent hover:underline" href={app.url} target="_blank" rel="noreferrer">{app.url}</a>
            {:else}
              <p class="text-sm text-gray-400">{app.exposure?.subdomain || 'cluster-internal only'}</p>
            {/if}
            <p class="text-xs text-gray-500 mt-1">v{app.version} • {app.chart} • {app.namespace}</p>
            {#if app.lastError}<p class="text-xs text-amber-400 mt-1">{app.lastError}</p>{/if}
          </div>
          <div class="flex gap-2">
            <button class="btn btn-secondary" disabled={runningJobApps.has(app.name)} on:click={() => editingExposure = app.name}>Exposure</button>
            <button
              class="btn btn-secondary"
              disabled={app.orphaned || runningJobApps.has(app.name)}
              title={app.orphaned ? 'Orphaned releases cannot be reconfigured' : ''}
              on:click={() => editingConfig = app.name}
            >Configure</button>
            <button class="btn btn-danger" disabled={runningJobApps.has(app.name)} on:click={() => uninstallApp(app.name)}>Uninstall</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if editingExposure}
    <AppExposureModal appName={editingExposure} on:close={() => { editingExposure = null; loadApps(); }} />
  {/if}
  {#if editingConfig}
    <AppConfigureModal appName={editingConfig} on:close={() => { editingConfig = null; loadApps(); }} />
  {/if}
</div>
