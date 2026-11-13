<script lang="ts">
  import { modal } from '$lib/actions/modal';
  import { onMount, createEventDispatcher } from 'svelte';
  import AppIcon from './AppIcon.svelte';
  import SchemaForm from './SchemaForm.svelte';
  import ExposureForm from './ExposureForm.svelte';
  import EnvVarEditor from './EnvVarEditor.svelte';
  import type { EnvRow } from '$lib/appEnv';
  import { trackAppJob } from '$lib/stores/appJobs';

  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    internet: boolean;
    service?: string;
    port?: number;
    scheme?: string;
  }

  interface CatalogApp {
    name: string;
    displayName: string;
    description: string;
    icon: string;
    iconUrl: string;
    version: string;
    chart: string;
    source: string;
    channel: string;
    chartPath: string;
    schema: { properties: Record<string, any>; required?: string[] };
    defaultValues: Record<string, any>;
    exposure: Exposure;
    services: { name: string; port: number; scheme: string }[];
    capabilities?: string[];
    privileged?: boolean;
  }

  export let appName: string;

  let app: CatalogApp | null = null;
  let loading = true;
  let installing = false;
  let error = '';
  let values: Record<string, any> = {};
  let exposure: Exposure = { subdomain: '', tls: true, auth: true, localOnly: false, internet: true };
  let baseDomain = '';
  let selectableDomains: string[] = [];
  let ssoDomains: string[] = [];
  let appRecordDomains = new Set<string>();
  let confirmed = false;
  let unsafeConfirmed = false;
  let privilegedConfirmed = false;
  let managedDatasets: { name: string; hostPath: string; shared: boolean; attachedTo?: string[] }[] = [];
  let selectedDatasets: string[] = [];
  let storagePools: string[] = [];
  let selectedPool = '';
  let activeStep: 'config' | 'exposure' | 'review' | 'confirm' = 'config';
  let env: EnvRow[] = [];
  let envError = '';
  // Naslos datasets are group-owned by `naslos_users` (10000). The schema's
  // upstream default (1000) cannot read them, so offer the platform groups.
  const groupOptions = [
    { value: 10000, label: 'naslos_users (10000) — dataset access' },
    { value: 1000, label: 'default (1000)' }
  ];
  // Apply the dataset owner once when the first dataset is selected. The user
  // can still change PUID/PGID afterwards.
  let datasetOwnerApplied = false;

  function datasetBlocked(dataset: { name: string; shared: boolean; attachedTo?: string[] }): boolean {
    return !dataset.shared && (dataset.attachedTo?.length ?? 0) > 0 && !(dataset.attachedTo ?? []).includes(appName);
  }

  $: sharedSelected = managedDatasets.filter((d) => d.shared && selectedDatasets.includes(d.name));
  $: if (app && selectedDatasets.length > 0 && !datasetOwnerApplied) {
    datasetOwnerApplied = true;
    const props = app.schema?.properties ?? {};
    const next = { ...values };
    if (props.pgid && Number(next.pgid ?? 0) === Number(props.pgid.default ?? 0)) next.pgid = 10000;
    if (props.puid && Number(next.puid ?? 0) === Number(props.puid.default ?? 0)) next.puid = 10001;
    values = next;
  }

  const dispatch = createEventDispatcher();

  $: envSupported = app?.capabilities?.includes('env') ?? false;

  /// Build the request payload, validating names and values client-side.
  function envPayload(): { name: string; value: string | null; secret: boolean }[] | null {
    envError = '';
    if (!envSupported) return [];
    const out: { name: string; value: string | null; secret: boolean }[] = [];
    const seen = new Set<string>();
    for (const row of env) {
      const name = row.name.trim();
      if (!name) {
        envError = 'Every environment variable needs a name.';
        return null;
      }
      if (name.length > 128 || !/^[A-Za-z_][A-Za-z0-9_.-]*$/.test(name)) {
        envError = `Invalid environment variable name ${name}.`;
        return null;
      }
      if (name.startsWith('NASLOS_')) {
        envError = `Environment variable ${name} uses the reserved NASLOS_ prefix.`;
        return null;
      }
      if (seen.has(name)) {
        envError = `Environment variable ${name} is defined twice.`;
        return null;
      }
      seen.add(name);
      if (row.value === null || row.value === '') {
        envError = `Environment variable ${name} needs a value.`;
        return null;
      }
      out.push({ name, value: row.value, secret: row.secret });
    }
    return out;
  }

  async function loadApp() {
    try {
      const [res, registryRes, poolsRes] = await Promise.all([
        fetch(`/api/catalog/${encodeURIComponent(appName)}`),
        fetch('/api/datasets/registry'),
        fetch('/api/volumes/zfs')
      ]);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      if (registryRes.ok) managedDatasets = await registryRes.json();
      if (poolsRes.ok) {
        const pools = await poolsRes.json();
        storagePools = Array.isArray(pools)
          ? pools.map((pool: any) => pool.name).filter((name: any) => typeof name === 'string')
          : [];
      }
      app = await res.json();
      if (app) {
        values = { ...app.defaultValues };
        exposure = {
          subdomain: app.exposure?.subdomain || app.name,
          tls: app.exposure?.tls ?? true,
          auth: app.exposure?.auth ?? true,
          localOnly: app.exposure?.localOnly ?? false,
          internet: app.exposure?.internet ?? true,
          service: app.services?.[0]?.name,
          port: app.services?.[0]?.port,
          scheme: app.services?.[0]?.scheme || 'http'
        };
        // A catalog entry with neither auth nor TLS should not be published
        // beyond the LAN by default.
        if (!exposure.tls && !exposure.auth) {
          exposure.localOnly = true;
        }
      }
      const domainRes = await fetch('/api/domains');
      if (domainRes.ok) {
        const domainData = await domainRes.json();
        baseDomain = domainData.baseDomain || '';
        const domains = Array.isArray(domainData.domains) ? domainData.domains : [];
        selectableDomains =
          Array.isArray(domainData.selectableDomains) && domainData.selectableDomains.length > 0
            ? domainData.selectableDomains
            : [baseDomain, ...domains.map((d: any) => d.baseDomain)].filter(Boolean);
        // Older APIs do not return ssoDomains; the primary is always SSO.
        ssoDomains =
          Array.isArray(domainData.ssoDomains) && domainData.ssoDomains.length > 0
            ? domainData.ssoDomains
            : [baseDomain].filter(Boolean);
        appRecordDomains = new Set(
          domains
            .filter((domain: any) => domain.appRecordsDdnsEntry)
            .map((domain: any) => domain.baseDomain)
        );
      }
    } catch (e) {
      error = 'Failed to load app details: ' + e;
    } finally {
      loading = false;
    }
  }

  async function install() {
    const submittedEnv = envPayload();
    if (submittedEnv === null) {
      error = envError;
      return;
    }
    installing = true;
    error = '';
    try {
      const res = await fetch('/api/apps', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: appName,
          values,
          exposure,
          baseDomain,
          confirmed: true,
          privilegedConfirmed,
          datasets: selectedDatasets,
          pool: selectedPool,
          env: submittedEnv
        })
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Install failed (HTTP ${res.status})`);
      }
      // The install runs as a background job: close the modal immediately and
      // let the global job drawer/toast follow it.
      const data = await res.json();
      if (data.jobId) trackAppJob(data.jobId);
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      installing = false;
    }
  }

  $: valueEntries = Object.entries(values).filter(([_, v]) => typeof v !== 'object');
  // Review must not print a password the operator just typed.
  $: secretKeys = new Set(
    Object.entries(app?.schema?.properties ?? {})
      .filter(([, prop]: [string, any]) => prop?.format === 'password' || prop?.writeOnly === true)
      .map(([key]) => key)
  );
  $: unsafeExposure =
    !!exposure.subdomain && (!exposure.tls || !exposure.auth);
  $: envReview = env
    .filter((row) => row.name.trim())
    .map((row) => `${row.name.trim()}=${row.secret ? '••••••' : (row.value ?? '')}`)
    .join(', ');
  onMount(loadApp);
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" use:modal={{ onClose: () => dispatch('close') }} on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-2xl max-h-[90vh] overflow-hidden flex flex-col">
    {#if loading}
      <div class="p-12 text-center text-gray-400">Loading app...</div>
    {:else if app}
      <div class="p-6 border-b border-naslos-border flex items-center gap-4">
        <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt={app.displayName} />
        <div class="flex-1">
          <h2 class="text-xl font-bold">{app.displayName}</h2>
          <p class="text-sm text-gray-400">{app.description}</p>
        </div>
        <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>×</button>
      </div>
      <div class="px-6 pt-4 flex gap-4">
        {#each [['config', 'Configuration'], ['exposure', 'Exposure'], ['review', 'Review'], ['confirm', 'Confirm']] as [step, label]}
          <button
            class={`text-sm font-medium pb-2 border-b-2 ${activeStep === step ? 'border-naslos-primary text-naslos-primary' : 'border-transparent text-gray-400'}`}
            on:click={() => activeStep = step as typeof activeStep}
          >{label}</button>
        {/each}
      </div>
      <div class="flex-1 overflow-y-auto p-6">
        {#if activeStep === 'config'}
          <SchemaForm {app} bind:values {groupOptions} />
          {#if envSupported}
            <details class="mt-4 border border-naslos-border rounded-lg p-4" open={env.length > 0}>
              <summary class="cursor-pointer text-sm font-medium">
                Environment variables{env.length ? ` (${env.length})` : ''}
              </summary>
              <div class="mt-3">
                <EnvVarEditor bind:env requireValues />
              </div>
            </details>
          {/if}
          <div class="mt-4">
            <span class="label">Datasets</span>
            {#if managedDatasets.length === 0}
              <p class="text-xs text-gray-500">
                No datasets are registered yet. Register one under Datasets to attach it to this app.
              </p>
            {:else}
              <div class="space-y-2">
                {#each managedDatasets as dataset (dataset.name)}
                  <label class="flex items-start gap-2 cursor-pointer">
                    <input
                      type="checkbox"
                      class="mt-1"
                      value={dataset.name}
                      bind:group={selectedDatasets}
                      disabled={datasetBlocked(dataset)}
                    />
                    <span class="text-sm">
                      <span class="font-mono">{dataset.name}</span> — {dataset.hostPath}
                      ({dataset.shared ? 'shared' : 'private'})
                      {#if datasetBlocked(dataset)}
                        <span class="text-amber-300">attached to {dataset.attachedTo?.join(', ')}</span>
                      {/if}
                    </span>
                  </label>
                {/each}
              </div>
              <p class="text-xs text-gray-500 mt-1">
                Each selected dataset is mounted at <span class="font-mono">/datasets/&lt;name&gt;</span>. A private
                dataset is attached to this app only; a shared dataset can be attached to several apps.
                Datasets are owned by group <span class="font-mono">naslos_users (10000)</span>; selecting one sets
                PUID/PGID to 10001/10000 so the app can read it.
              </p>
              {#each sharedSelected as dataset (dataset.name)}
                <p role="alert" class="text-xs text-amber-300 mt-1">
                  Dataset <span class="font-mono">{dataset.name}</span> is shared: every app attached to it can read
                  and modify its data.
                </p>
              {/each}
            {/if}
          </div>
          <div class="mt-4">
            <label class="label" for="install-pool">Storage pool</label>
            <select id="install-pool" class="input w-full" bind:value={selectedPool}>
              <option value="">Default (local-path)</option>
              {#each storagePools as pool (pool)}
                <option value={pool}>{pool}</option>
              {/each}
            </select>
            <p class="text-xs text-gray-500 mt-1">
              App config, database and upload PVCs are created under /var/mnt/&lt;pool&gt;/apps.
              The dataset above is separate: it controls the app's media/data volume.
            </p>
          </div>
        {:else if activeStep === 'exposure'}
          <ExposureForm bind:exposure bind:baseDomain {selectableDomains} {ssoDomains} />
          {#if exposure.subdomain && appRecordDomains.has(baseDomain)}
            <p class="mt-4 p-3 rounded-lg border border-naslos-border bg-naslos-dark/40 text-xs text-gray-300">
              A CNAME <span class="font-mono">{exposure.subdomain}.{baseDomain}</span> will be created on
              this domain and removed when the app is uninstalled or the subdomain changes.
            </p>
          {/if}
        {:else if activeStep === 'review'}
          <div class="space-y-3">
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">App</span><span class="font-medium">{app.displayName}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Source</span><span class="font-medium text-sm">{app.source} / {app.channel}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Chart</span><span class="font-medium text-sm">{app.chart}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Version</span><span class="font-medium">{app.version}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Datasets</span><span class="font-medium text-sm text-right">{selectedDatasets.length ? selectedDatasets.map((name) => `${name} → /datasets/${name}`).join(', ') : 'none'}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Storage pool</span><span class="font-medium text-sm">{selectedPool || 'default (local-path)'}</span></div>
            {#if envReview}
              <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Environment</span><span class="font-medium text-sm text-right font-mono">{envReview}</span></div>
            {/if}
            <div class="flex justify-between py-2 border-b border-naslos-border">
              <span class="text-gray-400">Exposure</span>
              <span class="font-medium text-sm text-right">
                {exposure.subdomain ? `${exposure.subdomain}.${baseDomain}` : 'cluster-internal only'}<br />
                TLS {exposure.tls ? 'on' : 'off'} · Auth {exposure.auth ? 'on' : 'off'} · Local-only {exposure.localOnly ? 'on' : 'off'} · Internet {exposure.internet ? 'on' : 'off'}
              </span>
            </div>
            {#each valueEntries as [key, value]}<div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">{key}</span><span class="font-medium text-sm">{secretKeys.has(key) && value ? '••••••' : (value || '(empty)')}</span></div>{/each}
          </div>
        {:else}
          <div class="space-y-4">
            {#if unsafeExposure}
              <div class="p-4 rounded-lg border border-red-700 bg-red-900/30 text-red-200 text-sm">
                <p class="font-medium mb-1">This app will be exposed without full protection</p>
                <p>
                  {#if !exposure.auth}Without Naslos login (auth is off).{/if}
                  {#if !exposure.tls}Without TLS (traffic is cleartext).{/if}
                  Anything that can reach <span class="font-mono">{exposure.subdomain}.{baseDomain}</span>
                  can use its admin interface.
                </p>
              </div>
            {/if}
            <div class="p-4 rounded-lg border border-amber-700 bg-amber-900/30 text-amber-200 text-sm">
              <p class="font-medium mb-1">Third-party chart trust boundary</p>
              <p>
                This installs <span class="font-mono">{app.source}</span> /
                <span class="font-mono">{app.chartPath || app.chart}</span> into
                <span class="font-mono">{app.privileged ? 'the privileged app pool' : 'an app namespace'}</span>. The chart's manifests
                run in-cluster under the API's namespaced permissions. Charts that
                request the datasets volume can read every Naslos dataset mounted
                below the platform's datasets root.
              </p>
            </div>
            {#if app.privileged}
              <div class="p-4 rounded-lg border border-red-700 bg-red-900/30 text-red-200 text-sm">
                <p class="font-medium mb-1">This app requests privileged mode</p>
                <p>
                  It will be installed into <span class="font-mono">naslos-apps-priv</span>,
                  the namespace that allows privileged pods and host access. Only install
                  it if you trust the chart source completely.
                </p>
              </div>
            {/if}
            <label class="flex items-start gap-3 cursor-pointer">
              <input
                type="checkbox"
                class="mt-1"
                bind:checked={confirmed}
              />
              <span>I have reviewed the source and confirm this install.</span>
            </label>
            {#if unsafeExposure}
              <label class="flex items-start gap-3 cursor-pointer">
                <input type="checkbox" class="mt-1" bind:checked={unsafeConfirmed} />
                <span class="font-medium text-red-300">
                  I understand this app is reachable without {#if !exposure.auth}Naslos login{/if}{#if !exposure.auth && !exposure.tls} and {/if}{#if !exposure.tls}encryption{/if}.
                </span>
              </label>
            {/if}
            {#if app.privileged}
              <label class="flex items-start gap-3 cursor-pointer">
                <input
                  type="checkbox"
                  class="mt-1"
                  bind:checked={privilegedConfirmed}
                />
                <span class="font-medium">I understand this app runs privileged and may access the host.</span>
              </label>
            {/if}
          </div>
        {/if}
        {#if error}<div role="alert" class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
      </div>
      <div class="p-6 border-t border-naslos-border flex justify-between">
        <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
        <div class="flex gap-3">
          {#if activeStep === 'config'}<button class="btn btn-primary" on:click={() => activeStep = 'exposure'}>Exposure →</button>
          {:else if activeStep === 'exposure'}<button class="btn btn-secondary" on:click={() => activeStep = 'config'}>← Back</button><button class="btn btn-primary" on:click={() => activeStep = 'review'}>Review →</button>
          {:else if activeStep === 'review'}<button class="btn btn-secondary" on:click={() => activeStep = 'exposure'}>← Back</button><button class="btn btn-primary" on:click={() => activeStep = 'confirm'}>Confirm →</button>
          {:else}<button class="btn btn-secondary" on:click={() => activeStep = 'review'}>← Back</button><button class="btn btn-primary" on:click={install} disabled={installing || !confirmed || (unsafeExposure && !unsafeConfirmed) || (app.privileged && !privilegedConfirmed)}>{installing ? 'Starting...' : 'Install'}</button>{/if}
        </div>
      </div>
    {/if}
  </div>
</div>
