<script lang="ts">
  import { onMount } from 'svelte';
  import AppExposureModal from './AppExposureModal.svelte';
  import AppConfigureModal from './AppConfigureModal.svelte';
  import InstalledAppRow from './InstalledAppRow.svelte';
  import AppDetailDrawer from './AppDetailDrawer.svelte';
  import AppUpdateModal from './AppUpdateModal.svelte';
  import { appJobs } from '$lib/stores/appJobs';
  import { appUpdates, refreshAppUpdates, startAppUpdates } from '$lib/stores/appUpdates';
  import { formatBytes, formatBytesPerSec, formatMillicores } from '$lib/format';
  import type { AppView, AppUsage, AppUsageHistory, UsageReport } from '$lib/appTypes';

  let apps: AppView[] = [];
  let loading = true;
  let error = '';
  let selected: AppView | null = null;
  let editingExposure: string | null = null;
  let editingConfig: string | null = null;
  let updating: AppView | null = null;
  // Apps with a job currently running: their actions are disabled and a badge
  // marks them as in-flight.
  let runningJobApps = new Set<string>();
  let previousRunning = new Set<string>();

  // Live usage from `/api/apps/usage` plus a client-side 5-minute history.
  let usageAvailable = false;
  let usageError = '';
  let usageByName: Record<string, AppUsage> = {};
  let historyByName: Record<string, AppUsageHistory> = {};
  let usageInFlight = false;
  let usageSeq = 0;
  let usageTimer: ReturnType<typeof setInterval> | null = null;

  let refreshing = false;
  let refreshMessage = '';
  let refreshError = '';
  let lastUpdatesSignature: string | null = null;

  async function loadApps(initial = false) {
    if (initial) loading = true;
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
      // Keep the drawer in sync, and close it when the app disappeared.
      if (selected) {
        selected = apps.find((a) => a.name === selected!.name) ?? null;
      }
    }
  }

  function pushSample(values: number[], value: number | null | undefined): number[] {
    if (value == null || !Number.isFinite(value)) return values;
    return [...values, value].slice(-60);
  }

  function applyUsage(report: UsageReport) {
    const next: Record<string, AppUsage> = {};
    for (const usage of report.apps ?? []) {
      next[usage.name] = usage;
    }
    usageByName = next;

    const nextHistory: Record<string, AppUsageHistory> = {};
    for (const [name, usage] of Object.entries(next)) {
      const previous = historyByName[name] ?? { cpu: [], memory: [], rx: [], tx: [] };
      nextHistory[name] = {
        cpu: pushSample(previous.cpu, usage.cpuMillicores),
        memory: pushSample(previous.memory, usage.memoryBytes),
        rx: pushSample(previous.rx, usage.networkRxBytesPerSec),
        tx: pushSample(previous.tx, usage.networkTxBytesPerSec)
      };
    }
    historyByName = nextHistory;
  }

  async function pollUsage() {
    if (typeof document !== 'undefined' && document.visibilityState !== 'visible') return;
    if (usageInFlight) return;
    usageInFlight = true;
    const seq = ++usageSeq;
    try {
      const res = await fetch('/api/apps/usage');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const report = (await res.json()) as UsageReport;
      // A slower earlier poll must not overwrite a newer sample.
      if (seq !== usageSeq) return;
      usageAvailable = Boolean(report.available);
      usageError = report.error || '';
      if (usageAvailable) {
        applyUsage(report);
      } else {
        usageByName = {};
      }
    } catch (e) {
      if (seq === usageSeq) {
        usageAvailable = false;
        usageError = `Usage is unavailable: ${e}`;
        usageByName = {};
      }
    } finally {
      usageInFlight = false;
    }
  }

  async function checkNow() {
    if (refreshing) return;
    refreshing = true;
    refreshMessage = '';
    refreshError = '';
    try {
      const res = await fetch('/api/sources/refresh', { method: 'POST' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        const errors = Array.isArray(data.errors) ? data.errors.join('; ') : data.error;
        refreshError = errors || `Refresh failed (HTTP ${res.status})`;
      } else {
        refreshMessage = `Catalog refreshed — ${data.catalog ?? 0} apps available`;
      }
      // The updates store subscription reloads the list when the catalog
      // signature changed; refreshing it first avoids a second /api/apps read.
      await refreshAppUpdates();
      await pollUsage();
    } catch (e) {
      refreshError = 'Refresh failed: ' + e;
    } finally {
      refreshing = false;
    }
  }

  $: updateCount = $appUpdates.count;
  $: aggregate = (() => {
    let cpu = 0;
    let memory = 0;
    let rx = 0;
    let tx = 0;
    for (const usage of Object.values(usageByName)) {
      if (usage.cpuMillicores != null) cpu += usage.cpuMillicores;
      if (usage.memoryBytes != null) memory += usage.memoryBytes;
      if (usage.networkRxBytesPerSec != null) rx += usage.networkRxBytesPerSec;
      if (usage.networkTxBytesPerSec != null) tx += usage.networkTxBytesPerSec;
    }
    return { cpu, memory, rx, tx };
  })();

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

  const unsubscribeUpdates = appUpdates.subscribe((updates) => {
    const signature = updates.apps.map((a) => `${a.name}:${a.latestVersion}`).join(',');
    if (lastUpdatesSignature === null) {
      lastUpdatesSignature = signature;
      return;
    }
    if (signature !== lastUpdatesSignature) {
      lastUpdatesSignature = signature;
      if (apps.length > 0) loadApps();
    }
  });

  function onVisibility() {
    if (document.visibilityState === 'visible') {
      void refreshAppUpdates();
      void pollUsage();
    }
  }

  function openFromDrawer(kind: 'exposure' | 'config') {
    if (!selected) return;
    const name = selected.name;
    selected = null;
    if (kind === 'exposure') editingExposure = name;
    else editingConfig = name;
  }

  function openUpdateFromDrawer() {
    if (!selected) return;
    updating = selected;
    selected = null;
  }

  onMount(() => {
    const stopUpdates = startAppUpdates();
    document.addEventListener('visibilitychange', onVisibility);
    void loadApps(true);
    void pollUsage();
    usageTimer = setInterval(() => void pollUsage(), 5000);
    return () => {
      unsubscribeJobs();
      unsubscribeUpdates();
      stopUpdates();
      document.removeEventListener('visibilitychange', onVisibility);
      if (usageTimer !== null) clearInterval(usageTimer);
    };
  });
</script>

<div>
  {#if error}
    <div role="alert" class="card border-red-700 bg-red-900/30 text-red-300 p-4 mb-4">{error}</div>
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
    <div class="flex flex-wrap items-center justify-between gap-3 mb-3">
      <div class="text-sm text-gray-400">
        <span class="font-medium text-gray-200">{apps.length} apps</span>
        <span class="mx-1">·</span>
        <span class:text-amber-300={updateCount > 0}>{updateCount} updates</span>
        {#if usageAvailable}
          <span class="mx-1">·</span>
          <span class="tabular-nums">
            CPU {formatMillicores(aggregate.cpu)} · RAM {formatBytes(aggregate.memory)} · ↓ {formatBytesPerSec(
              aggregate.rx
            )}
            ↑ {formatBytesPerSec(aggregate.tx)}
          </span>
        {/if}
      </div>
      <div class="flex items-center gap-2">
        {#if refreshMessage}
          <span class="text-xs text-green-400" role="status">{refreshMessage}</span>
        {/if}
        {#if refreshError}
          <span class="text-xs text-red-300" role="alert">{refreshError}</span>
        {/if}
        <button class="btn btn-secondary text-sm" disabled={refreshing} on:click={checkNow}>
          {refreshing ? 'Checking…' : 'Check now'}
        </button>
      </div>
    </div>

    {#if !usageAvailable && usageError}
      <p class="text-xs text-gray-500 mb-3">{usageError}</p>
    {/if}

    <div class="card p-0 divide-y divide-naslos-border overflow-hidden">
      {#each apps as app (app.name)}
        <InstalledAppRow
          {app}
          usage={usageByName[app.name]}
          jobRunning={runningJobApps.has(app.name)}
          on:open={() => (selected = app)}
          on:update={() => (updating = app)}
        />
      {/each}
    </div>
  {/if}

  {#if selected}
    <AppDetailDrawer
      app={selected}
      usage={usageByName[selected.name]}
      history={historyByName[selected.name]}
      jobRunning={runningJobApps.has(selected.name)}
      on:close={() => (selected = null)}
      on:update={openUpdateFromDrawer}      on:configure={() => openFromDrawer('config')}
      on:exposure={() => openFromDrawer('exposure')}
    />
  {/if}

  {#if updating}
    <AppUpdateModal app={updating} on:close={() => (updating = null)} />
  {/if}
  {#if editingExposure}
    <AppExposureModal
      appName={editingExposure}
      on:close={() => {
        editingExposure = null;
        loadApps();
      }}
    />
  {/if}
  {#if editingConfig}
    <AppConfigureModal
      appName={editingConfig}
      on:close={() => {
        editingConfig = null;
        loadApps();
      }}
    />
  {/if}
</div>
