<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import AsyncState from './AsyncState.svelte';
  import Button from './Button.svelte';
  import Modal from './Modal.svelte';
  import AppInstallConfigStep from './AppInstallConfigStep.svelte';
  import AppInstallExposureStep from './AppInstallExposureStep.svelte';
  import AppInstallReviewStep from './AppInstallReviewStep.svelte';
  import AppInstallConfirmStep from './AppInstallConfirmStep.svelte';
  import AppIcon from './AppIcon.svelte';
  import type { EnvRow, ManagedDataset } from '$lib/appEnv';
  import { validateEnvRows } from '$lib/appEnv';
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
  // Optional source state: a failed registry/pool/domain GET must be shown as
  // an error with Retry, never as an empty "none available" list.
  let datasetsLoaded = false;
  let poolsLoaded = false;
  let domainsLoaded = false;
  let datasetsError = '';
  let poolsError = '';
  let domainsError = '';
  let confirmed = false;
  let unsafeConfirmed = false;
  let privilegedConfirmed = false;
  let managedDatasets: ManagedDataset[] = [];
  let selectedDatasets: string[] = [];
  let storagePools: string[] = [];
  let selectedPool = '';
  let activeStep: 'config' | 'exposure' | 'review' | 'confirm' = 'config';
  let env: EnvRow[] = [];

  const groupOptions = [
    { value: 10000, label: 'naslos_users (10000) — dataset access' },
    { value: 1000, label: 'default (1000)' }
  ];

  const dispatch = createEventDispatcher();

  $: envSupported = app?.capabilities?.includes('env') ?? false;
  $: unsafeExposure = !!exposure.subdomain && (!exposure.tls || !exposure.auth);
  $: canInstall =
    !installing &&
    confirmed &&
    (!unsafeExposure || unsafeConfirmed) &&
    (!app?.privileged || privilegedConfirmed);
  // Review must not print a password the operator just typed.
  $: secretKeys = new Set(
    Object.entries(app?.schema?.properties ?? {})
      .filter(([, prop]: [string, any]) => prop?.format === 'password' || prop?.writeOnly === true)
      .map(([key]) => key)
  );
  $: envReview = env
    .filter((row) => row.name.trim())
    .map((row) => `${row.name.trim()}=${row.secret ? '••••••' : (row.value ?? '')}`)
    .join(', ');

  async function loadDatasets() {
    datasetsError = '';
    try {
      const res = await fetch('/api/datasets/registry');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      managedDatasets = Array.isArray(data) ? data : [];
      datasetsLoaded = true;
    } catch (e) {
      datasetsError = e instanceof Error ? e.message : String(e);
    }
  }

  async function loadPools() {
    poolsError = '';
    try {
      const res = await fetch('/api/volumes/zfs');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const pools = await res.json();
      storagePools = Array.isArray(pools)
        ? pools.map((pool: any) => pool.name).filter((name: any) => typeof name === 'string')
        : [];
      poolsLoaded = true;
    } catch (e) {
      poolsError = e instanceof Error ? e.message : String(e);
    }
  }

  async function loadDomains() {
    domainsError = '';
    try {
      const res = await fetch('/api/domains');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const domainData = await res.json();
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
      domainsLoaded = true;
    } catch (e) {
      domainsError = e instanceof Error ? e.message : String(e);
    }
  }

  async function loadApp() {
    loading = true;
    error = '';
    // Optional endpoints load independently: one failing source must not take
    // the whole install form down or masquerade as an empty list.
    const optional = Promise.allSettled([loadDatasets(), loadPools(), loadDomains()]);
    try {
      const res = await fetch(`/api/catalog/${encodeURIComponent(appName)}`);
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
    } catch (e) {
      error = 'Failed to load app details: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      await optional;
      loading = false;
    }
  }

  async function install() {
    const submittedEnv = validateEnvRows(env, {
      supported: envSupported,
      requireValues: true
    });
    if (submittedEnv.rows === null) {
      error = submittedEnv.error;
      activeStep = 'config';
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
          env: submittedEnv.rows
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

  function onSubmit() {
    error = '';
    if (activeStep === 'config') activeStep = 'exposure';
    else if (activeStep === 'exposure') activeStep = 'review';
    else if (activeStep === 'review') activeStep = 'confirm';
    else if (canInstall) void install();
  }

  onMount(loadApp);
</script>

<Modal
  title={app ? app.displayName : `Install ${appName}`}
  size="lg"
  onClose={() => dispatch('close')}
>
  <AsyncState
    {loading}
    error={app ? '' : error}
    loadingText="Loading app..."
    onRetry={loadApp}
  >
    {#if app}
      {#if error}
        <p role="alert" class="callout callout-bad">{error}</p>
      {/if}
      <div class="mb-4 flex items-center gap-4">
        <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt={app.displayName} />
        <p class="text-sm text-gray-400">{app.description}</p>
      </div>
      <div class="mb-4 flex flex-wrap gap-4 border-b border-naslos-border">
        {#each [['config', 'Configuration'], ['exposure', 'Exposure'], ['review', 'Review'], ['confirm', 'Confirm']] as [step, label]}
          <button
            type="button"
            class={`text-sm font-medium pb-2 border-b-2 ${activeStep === step ? 'border-naslos-primary text-naslos-link' : 'border-transparent text-gray-400'}`}
            on:click={() => {
              error = '';
              activeStep = step as typeof activeStep;
            }}
          >{label}</button>
        {/each}
      </div>

      <form id="install-form" on:submit|preventDefault={onSubmit}>
        {#if activeStep === 'config'}
          <AppInstallConfigStep
            {app}
            bind:values
            bind:selectedDatasets
            bind:selectedPool
            {managedDatasets}
            {datasetsLoaded}
            {datasetsError}
            {storagePools}
            {poolsLoaded}
            {poolsError}
            bind:env
            {groupOptions}
            retryDatasets={loadDatasets}
            retryPools={loadPools}
          />
        {:else if activeStep === 'exposure'}
          <AppInstallExposureStep
            bind:exposure
            bind:baseDomain
            {selectableDomains}
            {ssoDomains}
            {appRecordDomains}
            {domainsLoaded}
            {domainsError}
            retryDomains={loadDomains}
            on:change={() => (unsafeConfirmed = false)}
          />
        {:else if activeStep === 'review'}
          <AppInstallReviewStep
            {app}
            {values}
            {selectedDatasets}
            {selectedPool}
            {exposure}
            {baseDomain}
            {envReview}
          />
        {:else}
          <AppInstallConfirmStep
            {app}
            {exposure}
            {baseDomain}
            {unsafeExposure}
            bind:confirmed
            bind:unsafeConfirmed
            bind:privilegedConfirmed
          />
          {#if envReview && !envSupported}
            <p class="text-xs text-gray-500 mt-4">Environment: <span class="font-mono">{envReview}</span></p>
          {/if}
        {/if}
      </form>
    {/if}
  </AsyncState>

  <svelte:fragment slot="footer">
    {#if !app}
      <Button variant="secondary" on:click={() => dispatch('close')}>Close</Button>
    {:else}
      <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
      {#if activeStep === 'config'}
        <Button type="submit" form="install-form">Exposure →</Button>
      {:else if activeStep === 'exposure'}
        <Button variant="secondary" on:click={() => (activeStep = 'config')}>← Back</Button>
        <Button type="submit" form="install-form">Review →</Button>
      {:else if activeStep === 'review'}
        <Button variant="secondary" on:click={() => (activeStep = 'exposure')}>← Back</Button>
        <Button type="submit" form="install-form">Confirm →</Button>
      {:else}
        <Button variant="secondary" on:click={() => (activeStep = 'review')}>← Back</Button>
        <Button
          type="submit"
          form="install-form"
          loading={installing}
          disabled={!canInstall}
        >
          {installing ? 'Starting...' : 'Install'}
        </Button>
      {/if}
    {/if}
  </svelte:fragment>
</Modal>
