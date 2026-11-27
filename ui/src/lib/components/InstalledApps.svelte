<script context="module" lang="ts">
  import type { AppView } from '$lib/appTypes';
  // The last list paints immediately when the tab remounts; the fetch
  // below revalidates it.
  let cachedApps: AppView[] | null = null;
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import AppExposureModal from './AppExposureModal.svelte';
  import AppConfigureModal from './AppConfigureModal.svelte';
  import AppDetailDrawer from './AppDetailDrawer.svelte';
  import AppUpdateModal from './AppUpdateModal.svelte';
  import AppUpdateAllModal from './AppUpdateAllModal.svelte';
  import AsyncState from './AsyncState.svelte';
  import Button from './Button.svelte';
  import FormField from './FormField.svelte';
  import InstalledAppRow from './InstalledAppRow.svelte';
  import { appJobs } from '$lib/stores/appJobs';
  import { appUpdates, refreshAppUpdates, startAppUpdates } from '$lib/stores/appUpdates';
  import { formatBytes, formatBytesPerSec, formatMillicores } from '$lib/format';
  import type { AppUsage, AppUsageHistory, UsageReport, AppNamespaces } from '$lib/appTypes';

  let apps: AppView[] = cachedApps ?? [];
  let loading = cachedApps === null;
  let appsInFlight: Promise<void> | null = null;
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

  // Bulk update: the modal receives a snapshot so enqueued jobs that start
  // running cannot shrink the target list while the loop runs.
  let bulkOpen = false;
  let bulkTargets: AppView[] = [];
  let bulkMessage = '';
  let bulkError = '';

  // Per-app namespace slots: the chart provisions a fixed pool and the owner
  // decides how many of those slots are usable. Raising it here takes effect
  // immediately; it can never go below the installed apps.
  let pools: AppNamespaces | null = null;
  let poolsError = '';
  let editingLimit = false;
  let limitDraft = 0;
  let savingLimit = false;
  let limitError = '';
  let limitMessage = '';

  async function loadPools() {
    try {
      const res = await fetch('/api/apps/namespaces');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      if (!data || typeof data !== 'object' || !data.baseline || !data.privileged) {
        throw new Error('unexpected namespace response');
      }
      pools = data as AppNamespaces;
      poolsError = '';
    } catch (e) {
      poolsError = `Namespace slots are unavailable: ${e}`;
      pools = null;
    }
  }

  function startLimitEdit() {
    if (!pools) return;
    limitDraft = pools.baseline.limit;
    limitError = '';
    limitMessage = '';
    editingLimit = true;
  }

  function cancelLimitEdit() {
    editingLimit = false;
    limitError = '';
  }

  async function saveLimit() {
    if (!pools || savingLimit) return;
    savingLimit = true;
    limitError = '';
    limitMessage = '';
    try {
      const res = await fetch('/api/apps/namespaces', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ limit: Number(limitDraft) })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        limitError = data.error || `Could not change the limit (HTTP ${res.status})`;
      } else {
        pools = data as AppNamespaces;
        editingLimit = false;
        limitMessage = `Limit set to ${pools.baseline.limit} slots`;
      }
    } catch (e) {
      limitError = 'Could not change the limit: ' + e;
    } finally {
      savingLimit = false;
    }
  }

  async function fetchApps(initial: boolean) {
    if (initial) loading = true;
    error = '';
    try {
      const res = await fetch('/api/apps');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      apps = Array.isArray(data) ? data : [];
      cachedApps = apps;
    } catch (e) {
      error = 'Failed to load installed apps: ' + e;
      // Keep the rows painted from the cache; the callout reports the failure.
      if (apps.length === 0) apps = [];
    } finally {
      loading = false;
      // Keep the drawer in sync, and close it when the app disappeared.
      if (selected) {
        selected = apps.find((a) => a.name === selected!.name) ?? null;
      }
    }
  }

  function loadApps(initial = false): Promise<void> {
    if (appsInFlight) return appsInFlight;
    appsInFlight = fetchApps(initial).finally(() => {
      appsInFlight = null;
    });
    return appsInFlight;
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
  $: updateableApps = apps.filter(
    (app) => (app.updateAvailable || app.imageUpdateAvailable) && !runningJobApps.has(app.name)
  );
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
    if (finished) {
      loadApps();
      // The record's chart version changes when an upgrade succeeds, so the
      // update badge should clear without waiting for the next 60s poll.
      void refreshAppUpdates();
    }
  });

  const unsubscribeUpdates = appUpdates.subscribe((updates) => {
    const signature = updates.apps
      .map((a) => `${a.name}:${a.latestVersion}:${a.imageUpdate ? 1 : 0}`)
      .join(',');
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

  function openBulkUpdate() {
    if (bulkOpen) return;
    bulkTargets = updateableApps.slice();
    if (bulkTargets.length === 0) return;
    bulkMessage = '';
    bulkError = '';
    bulkOpen = true;
  }

  function onBulkDone(event: CustomEvent) {
    bulkOpen = false;
    const { started, failed } = event.detail as {
      started: string[];
      failed: { name: string; error: string }[];
    };
    if (started.length > 0) {
      bulkMessage = `Started ${started.length} update${started.length === 1 ? '' : 's'}`;
    }
    if (failed.length > 0) {
      bulkError =
        `${failed.length} update${failed.length === 1 ? '' : 's'} could not start: ` +
        failed.map((failure) => `${failure.name} (${failure.error})`).join('; ');
    }
    void refreshAppUpdates();
  }

  onMount(() => {
    const stopUpdates = startAppUpdates();
    document.addEventListener('visibilitychange', onVisibility);
    void Promise.all([loadApps(true), loadPools(), pollUsage()]);
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

<AsyncState
  loading={loading && apps.length === 0}
  error={error}
  empty={!loading && apps.length === 0}
  loadingText="Loading installed apps..."
  emptyTitle="No Apps Installed"
  emptyDetail="Browse the catalog to install your first app."
  emptyIcon="package"
  onRetry={() => loadApps(true)}
>
  <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
    <div class="text-sm text-gray-400">
      <span class="font-medium text-gray-200">{apps.length} apps</span>
      <span class="mx-1">·</span>
      <span class:text-amber-300={updateCount > 0}>{updateCount} {updateCount === 1 ? 'update' : 'updates'}</span>
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
    <div class="flex flex-wrap items-center gap-2">
      {#if bulkMessage}
        <span class="text-xs text-green-400" role="status">{bulkMessage}</span>
      {/if}
      {#if bulkError}
        <span class="text-xs text-red-300" role="alert">{bulkError}</span>
      {/if}
      {#if refreshMessage}
        <span class="text-xs text-green-400" role="status">{refreshMessage}</span>
      {/if}
      {#if refreshError}
        <span class="text-xs text-red-300" role="alert">{refreshError}</span>
      {/if}
      {#if updateableApps.length > 0}
        <Button size="sm" disabled={bulkOpen} on:click={openBulkUpdate}>
          Update all ({updateableApps.length})
        </Button>
      {/if}
      <Button variant="secondary" size="sm" loading={refreshing} on:click={checkNow}>
        {refreshing ? 'Checking…' : 'Check now'}
      </Button>
    </div>
  </div>

  {#if !usageAvailable && usageError}
    <p class="text-xs text-gray-500 mb-3">{usageError}</p>
  {/if}

  {#if pools && pools.baseline.provisioned > 0}
    <div class="mb-2 flex flex-wrap items-center justify-between gap-2 text-sm">
      <span class="text-gray-400">
        Namespace slots
        <span class="text-gray-200 font-medium tabular-nums"
          >{pools.baseline.used}/{pools.baseline.limit}</span
        >
        used
        <span class="mx-1">·</span>
        <span class="text-gray-500">{pools.baseline.provisioned} provisioned</span>
        {#if pools.privileged.provisioned > 0}
          <span class="mx-1">·</span>
          <span class="text-gray-500"
            >privileged {pools.privileged.used}/{pools.privileged.provisioned}</span
          >
        {/if}
      </span>
      <div class="flex items-center gap-2">
        {#if limitMessage}
          <span class="text-xs text-green-400" role="status">{limitMessage}</span>
        {/if}
        <Button variant="secondary" size="sm" on:click={editingLimit ? cancelLimitEdit : startLimitEdit}>
          {editingLimit ? 'Close' : 'Adjust limit'}
        </Button>
      </div>
    </div>

    {#if editingLimit}
      <form class="card mb-3 flex flex-wrap items-end gap-3" on:submit|preventDefault={saveLimit}>
        <FormField label="Usable app namespace slots" id="app-slot-limit">
          <input
            id="app-slot-limit"
            class="input w-28"
            type="number"
            min={Math.max(1, pools.baseline.used)}
            max={pools.baseline.provisioned}
            bind:value={limitDraft}
          />
        </FormField>
        <p class="text-xs text-gray-500 max-w-md pb-2">
          Between {Math.max(1, pools.baseline.used)} (in use) and
          {pools.baseline.provisioned} (provisioned). Raising the limit takes effect
          immediately; more than {pools.baseline.provisioned} needs a platform upgrade.
        </p>
        {#if limitError}
          <p class="text-xs text-red-300 pb-2" role="alert">{limitError}</p>
        {/if}
        <Button
          type="submit"
          size="sm"
          loading={savingLimit}
          disabled={Number(limitDraft) < Math.max(1, pools.baseline.used)}
        >
          {savingLimit ? 'Saving…' : 'Save limit'}
        </Button>
      </form>
    {/if}
    {#if poolsError}
      <p class="text-xs text-amber-300 mb-2" role="alert">{poolsError}</p>
    {/if}
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
</AsyncState>

{#if selected}
  <AppDetailDrawer
    app={selected}
    usage={usageByName[selected.name]}
    history={historyByName[selected.name]}
    jobRunning={runningJobApps.has(selected.name)}
    on:close={() => (selected = null)}
    on:update={openUpdateFromDrawer}
    on:configure={() => openFromDrawer('config')}
    on:exposure={() => openFromDrawer('exposure')}
  />
{/if}

{#if updating}
  <AppUpdateModal app={updating} on:close={() => (updating = null)} />
{/if}
{#if bulkOpen}
  <AppUpdateAllModal
    apps={bulkTargets}
    on:close={() => (bulkOpen = false)}
    on:done={onBulkDone}
  />
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
