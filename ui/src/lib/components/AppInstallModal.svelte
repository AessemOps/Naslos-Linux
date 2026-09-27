<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import SchemaForm from './SchemaForm.svelte';
  import ExposureForm from './ExposureForm.svelte';
  import { trackAppJob } from '$lib/stores/appJobs';

  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    service?: string;
    port?: number;
    scheme?: string;
  }

  interface CatalogApp {
    name: string;
    displayName: string;
    description: string;
    icon: string;
    version: string;
    chart: string;
    source: string;
    channel: string;
    chartPath: string;
    schema: { properties: Record<string, any>; required?: string[] };
    defaultValues: Record<string, any>;
    exposure: Exposure;
    services: { name: string; port: number; scheme: string }[];
  }

  export let appName: string;

  let app: CatalogApp | null = null;
  let loading = true;
  let installing = false;
  let error = '';
  let values: Record<string, any> = {};
  let exposure: Exposure = { subdomain: '', tls: true, auth: true, localOnly: false };
  let baseDomain = '';
  let selectableDomains: string[] = [];
  let ssoDomains: string[] = [];
  let confirmed = false;
  let activeStep: 'config' | 'exposure' | 'review' | 'confirm' = 'config';

  const dispatch = createEventDispatcher();

  async function loadApp() {
    try {
      const res = await fetch(`/api/catalog/${appName}`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      app = await res.json();
      if (app) {
        values = { ...app.defaultValues };
        exposure = {
          subdomain: app.exposure?.subdomain || app.name,
          tls: app.exposure?.tls ?? true,
          auth: app.exposure?.auth ?? true,
          localOnly: app.exposure?.localOnly ?? false,
          service: app.services?.[0]?.name,
          port: app.services?.[0]?.port,
          scheme: app.services?.[0]?.scheme || 'http'
        };
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
      }
    } catch (e) {
      error = 'Failed to load app details: ' + e;
    } finally {
      loading = false;
    }
  }

  async function install() {
    installing = true;
    error = '';
    try {
      const res = await fetch('/api/apps', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: appName, values, exposure, baseDomain, confirmed: true })
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
  onMount(loadApp);
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-2xl max-h-[90vh] overflow-hidden flex flex-col">
    {#if loading}
      <div class="p-12 text-center text-gray-400">Loading app...</div>
    {:else if app}
      <div class="p-6 border-b border-naslos-border flex items-center gap-4">
        <div class="text-4xl">{app.icon}</div>
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
          <SchemaForm {app} bind:values />
        {:else if activeStep === 'exposure'}
          <ExposureForm bind:exposure bind:baseDomain {selectableDomains} {ssoDomains} />
        {:else if activeStep === 'review'}
          <div class="space-y-3">
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">App</span><span class="font-medium">{app.displayName}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Source</span><span class="font-medium text-sm">{app.source} / {app.channel}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Chart</span><span class="font-medium text-sm">{app.chart}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Version</span><span class="font-medium">{app.version}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border">
              <span class="text-gray-400">Exposure</span>
              <span class="font-medium text-sm text-right">
                {exposure.subdomain ? `${exposure.subdomain}.${baseDomain}` : 'cluster-internal only'}<br />
                TLS {exposure.tls ? 'on' : 'off'} · Auth {exposure.auth ? 'on' : 'off'} · Local-only {exposure.localOnly ? 'on' : 'off'}
              </span>
            </div>
            {#each valueEntries as [key, value]}<div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">{key}</span><span class="font-medium text-sm">{value || '(empty)'}</span></div>{/each}
          </div>
        {:else}
          <div class="space-y-4">
            <div class="p-4 rounded-lg border border-amber-700 bg-amber-900/30 text-amber-200 text-sm">
              <p class="font-medium mb-1">Third-party chart trust boundary</p>
              <p>
                This installs <span class="font-mono">{app.source}</span> /
                <span class="font-mono">{app.chartPath || app.chart}</span> into
                <span class="font-mono">naslos-apps</span>. The chart's manifests
                run in-cluster under the API's namespaced permissions.
              </p>
            </div>
            <label class="flex items-start gap-3 cursor-pointer">
              <input
                type="checkbox"
                class="mt-1"
                bind:checked={confirmed}
              />
              <span>I have reviewed the source and confirm this install.</span>
            </label>
          </div>
        {/if}
        {#if error}<div class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
      </div>
      <div class="p-6 border-t border-naslos-border flex justify-between">
        <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
        <div class="flex gap-3">
          {#if activeStep === 'config'}<button class="btn btn-primary" on:click={() => activeStep = 'exposure'}>Exposure →</button>
          {:else if activeStep === 'exposure'}<button class="btn btn-secondary" on:click={() => activeStep = 'config'}>← Back</button><button class="btn btn-primary" on:click={() => activeStep = 'review'}>Review →</button>
          {:else if activeStep === 'review'}<button class="btn btn-secondary" on:click={() => activeStep = 'exposure'}>← Back</button><button class="btn btn-primary" on:click={() => activeStep = 'confirm'}>Confirm →</button>
          {:else}<button class="btn btn-secondary" on:click={() => activeStep = 'review'}>← Back</button><button class="btn btn-primary" on:click={install} disabled={installing || !confirmed}>{installing ? 'Starting...' : 'Install'}</button>{/if}
        </div>
      </div>
    {/if}
  </div>
</div>
