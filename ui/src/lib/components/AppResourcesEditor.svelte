<script lang="ts">
  import Button from './Button.svelte';
  import FormField from './FormField.svelte';
  import { trackAppJob } from '$lib/stores/appJobs';
  import type { AppView, AppResourceQuota } from '$lib/appTypes';

  export let app: AppView;
  export let jobRunning = false;

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

  // Send the complete `resources` object, built from scratch, instead of a
  // sparse patch. A cleared field is simply absent from the object, so the API
  // replaces the stored `resources` value and the limit/request disappears;
  // no JSON null is needed (and the API rejects null quantities). Keys the
  // form does not edit are carried over unchanged.
  function buildResources(): Record<string, any> {
    const out: Record<string, any> = {};
    if (!resourcesFlat) {
      // Preserve root keys that are not per-component resources (the form
      // only edits components with requests/limits sections).
      for (const [key, value] of Object.entries(resourcesSource)) {
        if (!(isRecord(value) && hasSections(value))) out[key] = value;
      }
    }
    for (const group of resourceGroups) {
      const source: Record<string, any> = resourcesFlat
        ? resourcesSource
        : isRecord(resourcesSource[group.key])
          ? resourcesSource[group.key]
          : {};
      const target: Record<string, any> = {};
      for (const [key, value] of Object.entries(source)) {
        if (key !== 'requests' && key !== 'limits') target[key] = value;
      }
      const fields: [string, string, string][] = [
        ['requests', 'cpu', group.requestsCpu],
        ['requests', 'memory', group.requestsMemory],
        ['limits', 'cpu', group.limitsCpu],
        ['limits', 'memory', group.limitsMemory]
      ];
      for (const [section, field, raw] of fields) {
        const value = raw.trim();
        if (!isRecord(target[section])) target[section] = {};
        if (value) target[section][field] = value;
      }
      for (const section of ['requests', 'limits']) {
        if (!isRecord(target[section])) continue;
        const existing = isRecord(source[section]) ? source[section] : {};
        for (const [key, value] of Object.entries(existing)) {
          if (key !== 'cpu' && key !== 'memory') target[section][key] = value;
        }
        if (Object.keys(target[section]).length === 0) delete target[section];
      }
      if (resourcesFlat) Object.assign(out, target);
      else if (Object.keys(target).length > 0) out[group.key] = target;
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
      // Paint the value that was sent; a re-open/reload reads the persisted
      // record back once the upgrade job finishes.
      resourcesSource = resources;
      const parsed = groupsFor(resources);
      resourcesFlat = parsed.flat;
      resourceGroups = parsed.groups;
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
</script>

<section>
  <div class="flex items-center justify-between mb-2">
    <h3 class="text-xs uppercase tracking-wide text-gray-500">Resources</h3>
    <Button
      variant="ghost"
      size="sm"
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
    </Button>
  </div>
  <div class="space-y-2">
    {#each resourceGroups as group, index (group.key)}
      <div class="rounded-lg border border-naslos-border p-3">
        {#if resourceGroups.length > 1 || !resourcesFlat}
          <div class="text-sm font-medium mb-2">{group.label}</div>
        {/if}
        {#if editingResources}
          <div class="grid grid-cols-1 gap-2 sm:grid-cols-2">
            <FormField label="CPU request" id="res-{index}-requests-cpu">
              <input
                id="res-{index}-requests-cpu"
                class="input w-full"
                value={group.requestsCpu}
                placeholder="e.g. 100m"
                on:input={(e) => (group.requestsCpu = e.currentTarget.value)}
              />
            </FormField>
            <FormField label="Memory request" id="res-{index}-requests-memory">
              <input
                id="res-{index}-requests-memory"
                class="input w-full"
                value={group.requestsMemory}
                placeholder="e.g. 256Mi"
                on:input={(e) => (group.requestsMemory = e.currentTarget.value)}
              />
            </FormField>
            <FormField label="CPU limit" id="res-{index}-limits-cpu">
              <input
                id="res-{index}-limits-cpu"
                class="input w-full"
                value={group.limitsCpu}
                placeholder="e.g. 2"
                on:input={(e) => (group.limitsCpu = e.currentTarget.value)}
              />
            </FormField>
            <FormField label="Memory limit" id="res-{index}-limits-memory">
              <input
                id="res-{index}-limits-memory"
                class="input w-full"
                value={group.limitsMemory}
                placeholder="e.g. 2Gi"
                on:input={(e) => (group.limitsMemory = e.currentTarget.value)}
              />
            </FormField>
          </div>
        {:else}
          <div class="grid grid-cols-1 gap-1 text-xs tabular-nums sm:grid-cols-2 sm:gap-x-8 sm:gap-y-1">
            <div class="flex items-baseline justify-between gap-2">
              <span class="text-gray-500">CPU request</span>
              <span class="text-gray-200">{group.requestsCpu || '—'}</span>
            </div>
            <div class="flex items-baseline justify-between gap-2">
              <span class="text-gray-500">Memory request</span>
              <span class="text-gray-200">{group.requestsMemory || '—'}</span>
            </div>
            <div class="flex items-baseline justify-between gap-2">
              <span class="text-gray-500">CPU limit</span>
              <span class="text-gray-200">{group.limitsCpu || '—'}</span>
            </div>
            <div class="flex items-baseline justify-between gap-2">
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
      <Button size="sm" loading={savingResources} disabled={jobRunning} on:click={saveResources}>
        {savingResources ? 'Saving…' : 'Save resources'}
      </Button>
    </div>
  {/if}
</section>
