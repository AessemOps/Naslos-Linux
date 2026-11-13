<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { modal } from '$lib/actions/modal';
  import { trackAppJob } from '$lib/stores/appJobs';
  import AppIcon from './AppIcon.svelte';
  import AppLogs from './AppLogs.svelte';
  import Sparkline from './Sparkline.svelte';
  import { formatBytes, formatBytesPerSec, formatMillicores } from '$lib/format';
  import { statusClass, statusLabel } from '$lib/appStatus';
  import type { AppView, AppUsage, AppUsageHistory, AppResourceQuota } from '$lib/appTypes';

  export let app: AppView;
  export let usage: AppUsage | undefined = undefined;
  export let history: AppUsageHistory | undefined = undefined;
  export let jobRunning = false;

  const dispatch = createEventDispatcher();

  let uninstalling = false;
  let actionError = '';
  let dnsBusy = false;
  let dnsMessage = '';

  interface ResourceGroup {
    key: string;
    label: string;
    requestsCpu: string;
    requestsMemory: string;
    limitsCpu: string;
    limitsMemory: string;
  }

  let resourceGroups: ResourceGroup[] = [];
  let resourcesFlat = true;
  // The recorded `resources` value, kept so keys the form does not edit are
  // preserved when it is written back.
  let resourcesSource: Record<string, any> = {};
  let resourcesFor = '';
  let editingResources = false;
  let savingResources = false;
  let resourceError = '';
  let resourceMessage = '';
  let quota: AppResourceQuota | null = null;

  function isRecord(value: unknown): value is Record<string, any> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }

  function quantity(value: unknown): string {
    return typeof value === 'string' || typeof value === 'number' ? String(value) : '';
  }

  function hasSections(value: Record<string, any>): boolean {
    return isRecord(value.requests) || isRecord(value.limits);
  }

  function groupsFor(source: Record<string, any>): { flat: boolean; groups: ResourceGroup[] } {
    const components = Object.entries(source).filter(
      ([, value]) => isRecord(value) && hasSections(value)
    );
    const flat = hasSections(source) || components.length === 0;
    const entries: [string, Record<string, any>][] = flat ? [['', source]] : components;
    return {
      flat,
      groups: entries.map(([key, value]) => {
        const requests = isRecord(value.requests) ? value.requests : {};
        const limits = isRecord(value.limits) ? value.limits : {};
        return {
          key,
          label: key || 'App container',
          requestsCpu: quantity(requests.cpu),
          requestsMemory: quantity(requests.memory),
          limitsCpu: quantity(limits.cpu),
          limitsMemory: quantity(limits.memory)
        };
      })
    };
  }

  function resetResources() {
    const raw = app.values?.resources;
    resourcesSource = isRecord(raw) ? JSON.parse(JSON.stringify(raw)) : {};
    const parsed = groupsFor(resourcesSource);
    resourcesFlat = parsed.flat;
    resourceGroups = parsed.groups;
    editingResources = false;
    resourceError = '';
    resourceMessage = '';
  }

  function cancelResourceEdit() {
    const parsed = groupsFor(resourcesSource);
    resourcesFlat = parsed.flat;
    resourceGroups = parsed.groups;
    editingResources = false;
    resourceError = '';
  }

  const QUANTITY = /^([0-9]+(?:\.[0-9]*)?|\.[0-9]+)([numkMGTPE]|Ki|Mi|Gi|Ti|Pi|Ei)?$/;
  const QUANTITY_MULTIPLIERS: Record<string, number> = {
    '': 1,
    n: 1e-9,
    u: 1e-6,
    m: 1e-3,
    k: 1e3,
    M: 1e6,
    G: 1e9,
    T: 1e12,
    P: 1e15,
    E: 1e18,
    Ki: 1024,
    Mi: 1024 ** 2,
    Gi: 1024 ** 3,
    Ti: 1024 ** 4,
    Pi: 1024 ** 5,
    Ei: 1024 ** 6
  };

  function parseQuantity(raw: string): number | null {
    const text = raw.trim();
    if (!text) return 0;
    const match = QUANTITY.exec(text);
    if (!match) return null;
    const multiplier = QUANTITY_MULTIPLIERS[match[2] ?? ''];
    return multiplier === undefined ? null : Number(match[1]) * multiplier;
  }

  function draftTotals() {
    const totals = { requestsCpu: 0, requestsMemory: 0, limitsCpu: 0, limitsMemory: 0 };
    for (const group of resourceGroups) {
      const fields: [keyof typeof totals, string][] = [
        ['requestsCpu', group.requestsCpu],
        ['requestsMemory', group.requestsMemory],
        ['limitsCpu', group.limitsCpu],
        ['limitsMemory', group.limitsMemory]
      ];
      for (const [key, raw] of fields) {
        const value = parseQuantity(raw);
        if (value !== null) totals[key] += value;
      }
    }
    return totals;
  }

  function quotaViolation(): string {
    const hard = quota?.hard;
    if (!quota?.available || !hard) return '';
    const totals = draftTotals();
    const checks: [keyof typeof hard, number, string][] = [
      ['requests.cpu', totals.requestsCpu, 'CPU requests'],
      ['requests.memory', totals.requestsMemory, 'memory requests'],
      ['limits.cpu', totals.limitsCpu, 'CPU limits'],
      ['limits.memory', totals.limitsMemory, 'memory limits']
    ];
    for (const [key, total, label] of checks) {
      const allowed = hard[key];
      if (!allowed) continue;
      const limit = parseQuantity(allowed);
      if (limit !== null && total > limit + 1e-9) {
        return `${label} exceed the namespace quota (${allowed}). Lower the values or raise apps.isolation.quota with a platform upgrade.`;
      }
    }
    return '';
  }

  function validateResources(): string {
    for (const group of resourceGroups) {
      const fields: [string, string][] = [
        ['CPU request', group.requestsCpu],
        ['memory request', group.requestsMemory],
        ['CPU limit', group.limitsCpu],
        ['memory limit', group.limitsMemory]
      ];
      for (const [label, raw] of fields) {
        const text = raw.trim();
        if (text && !QUANTITY.test(text)) {
          return `${group.label}: ${label} “${text}” is not a Kubernetes quantity (e.g. 100m, 0.5, 256Mi).`;
        }
      }
    }
    return quotaViolation();
  }

  async function loadQuota() {
    const name = app.name;
    quota = null;
    try {
      const res = await fetch(`/api/apps/${encodeURIComponent(name)}/quota`);
      if (!res.ok) return;
      const data = (await res.json()) as AppResourceQuota;
      if (app.name === name) quota = data.available ? data : null;
    } catch {
      // The quota is advisory; the API still validates on upgrade.
    }
  }

  function buildResources(): Record<string, any> {
    const out: Record<string, any> = JSON.parse(JSON.stringify(resourcesSource));
    for (const group of resourceGroups) {
      let target: Record<string, any> = out;
      if (!resourcesFlat) {
        if (!isRecord(out[group.key])) out[group.key] = {};
        target = out[group.key];
      }
      const fields: [string, string, string][] = [
        ['requests', 'cpu', group.requestsCpu],
        ['requests', 'memory', group.requestsMemory],
        ['limits', 'cpu', group.limitsCpu],
        ['limits', 'memory', group.limitsMemory]
      ];
      for (const [section, field, raw] of fields) {
        const value = raw.trim();
        if (!isRecord(target[section])) {
          if (!value) continue;
          target[section] = {};
        }
        if (value) {
          target[section][field] = value;
        } else {
          delete target[section][field];
          if (Object.keys(target[section]).length === 0) delete target[section];
        }
      }
    }
    return out;
  }

  async function saveResources() {
    if (savingResources || jobRunning) return;
    const invalid = validateResources();
    if (invalid) {
      resourceError = invalid;
      return;
    }
    savingResources = true;
    resourceError = '';
    resourceMessage = '';
    try {
      const resources = buildResources();
      const res = await fetch(`/api/apps/${encodeURIComponent(app.name)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ values: { resources } })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      if (data.jobId) trackAppJob(data.jobId);
      resourcesSource = resources;
      editingResources = false;
      resourceMessage = 'Resource change queued — the app will restart.';
    } catch (e: any) {
      resourceError = e?.message || String(e);
    } finally {
      savingResources = false;
    }
  }

  $: if (app.name !== resourcesFor) {
    resourcesFor = app.name;
    resetResources();
    void loadQuota();
  }

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
    class="absolute inset-y-0 right-0 w-full max-w-xl bg-naslos-surface border-l border-naslos-border shadow-xl flex flex-col"
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

      <!-- Resources -->
      <section>
        <div class="flex items-center justify-between mb-2">
          <h3 class="text-xs uppercase tracking-wide text-gray-500">Resources</h3>
          <button
            type="button"
            class="text-xs px-2 py-1 rounded bg-naslos-dark border border-naslos-border hover:border-naslos-accent disabled:opacity-50"
            disabled={jobRunning || savingResources || app.orphaned}
            title={app.orphaned ? 'Orphaned releases cannot be reconfigured' : ''}
            on:click={editingResources
              ? cancelResourceEdit
              : () => {
                  editingResources = true;
                  resourceError = '';
                  resourceMessage = '';
                }}
          >
            {editingResources ? 'Cancel' : 'Adjust'}
          </button>
        </div>
        <div class="space-y-2">
          {#each resourceGroups as group (group.key)}
            <div class="rounded-lg border border-naslos-border p-3">
              {#if resourceGroups.length > 1 || !resourcesFlat}
                <div class="text-sm font-medium mb-2">{group.label}</div>
              {/if}
              {#if editingResources}
                <div class="grid grid-cols-2 gap-2">
                  <label class="text-xs text-gray-400">
                    CPU request
                    <input
                      class="input w-full"
                      value={group.requestsCpu}
                      placeholder="e.g. 100m"
                      on:input={(e) => (group.requestsCpu = e.currentTarget.value)}
                    />
                  </label>
                  <label class="text-xs text-gray-400">
                    Memory request
                    <input
                      class="input w-full"
                      value={group.requestsMemory}
                      placeholder="e.g. 256Mi"
                      on:input={(e) => (group.requestsMemory = e.currentTarget.value)}
                    />
                  </label>
                  <label class="text-xs text-gray-400">
                    CPU limit
                    <input
                      class="input w-full"
                      value={group.limitsCpu}
                      placeholder="e.g. 2"
                      on:input={(e) => (group.limitsCpu = e.currentTarget.value)}
                    />
                  </label>
                  <label class="text-xs text-gray-400">
                    Memory limit
                    <input
                      class="input w-full"
                      value={group.limitsMemory}
                      placeholder="e.g. 2Gi"
                      on:input={(e) => (group.limitsMemory = e.currentTarget.value)}
                    />
                  </label>
                </div>
              {:else}
                <div class="grid grid-cols-2 gap-2 text-xs tabular-nums">
                  <div>
                    <span class="text-gray-500">CPU request</span>
                    <span class="text-gray-200">{group.requestsCpu || '—'}</span>
                  </div>
                  <div>
                    <span class="text-gray-500">Memory request</span>
                    <span class="text-gray-200">{group.requestsMemory || '—'}</span>
                  </div>
                  <div>
                    <span class="text-gray-500">CPU limit</span>
                    <span class="text-gray-200">{group.limitsCpu || '—'}</span>
                  </div>
                  <div>
                    <span class="text-gray-500">Memory limit</span>
                    <span class="text-gray-200">{group.limitsMemory || '—'}</span>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        </div>
        {#if quota?.available && quota.hard}
          <p class="text-xs text-gray-500 mt-2">
            Namespace quota: {quota.hard['limits.cpu'] || '—'} CPU and {quota.hard['limits.memory'] || '—'} memory
            limits, {quota.hard['requests.cpu'] || '—'} CPU and {quota.hard['requests.memory'] || '—'} requests.
          </p>
        {/if}
        {#if editingResources}
          <p class="text-xs text-gray-500 mt-2">
            Applied with a Helm upgrade. Blank fields are omitted; CPU accepts fractions
            (<span class="font-mono">0.5</span>) or millicores (<span class="font-mono">500m</span>), memory a
            binary unit (<span class="font-mono">512Mi</span>).
          </p>
        {/if}
        {#if resourceError}
          <p class="text-xs text-red-300 mt-2 break-words" role="alert">{resourceError}</p>
        {/if}
        {#if resourceMessage}
          <p class="text-xs text-green-400 mt-2" role="status">{resourceMessage}</p>
        {/if}
        {#if editingResources}
          <div class="flex justify-end mt-2">
            <button
              class="btn btn-primary text-sm"
              type="button"
              disabled={savingResources || jobRunning}
              on:click={saveResources}
            >
              {savingResources ? 'Saving…' : 'Save resources'}
            </button>
          </div>
        {/if}
      </section>

      <!-- Live logs -->
      {#key app.name}
        <AppLogs {app} />
      {/key}

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
          <span class="text-gray-500">DNS</span>
          <div class="text-right min-w-0">
            {#if app.dns}
              <p class="font-mono text-xs truncate">{app.dns.fqdn} → {app.dns.target}</p>
              <p class="text-xs" class:text-emerald-300={app.dns.status === 'ok'} class:text-red-300={app.dns.status !== 'ok'}>
                {app.dns.status}{#if app.dns.error}: {app.dns.error}{/if}
              </p>
              <button class="text-xs text-naslos-accent hover:underline mt-1" on:click={retryDns} disabled={dnsBusy}>
                {dnsBusy ? 'Retrying…' : 'Retry'}
              </button>
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
