<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from './Button.svelte';
  import ExposureForm from './ExposureForm.svelte';

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

  export let exposure: Exposure;
  export let baseDomain = '';
  export let selectableDomains: string[] = [];
  export let ssoDomains: string[] = [];
  export let appRecordDomains = new Set<string>();
  export let domainsLoaded = false;
  export let domainsError = '';
  export let retryDomains: () => void = () => {};

  const dispatch = createEventDispatcher();
</script>

{#if domainsError}
  <div role="alert" class="callout callout-bad">
    <p>Domains are unavailable: {domainsError}.</p>
    <Button variant="secondary" size="sm" on:click={retryDomains}>Retry</Button>
  </div>
{:else if !domainsLoaded}
  <p class="text-sm text-gray-400">Loading domains…</p>
{:else}
  <ExposureForm
    bind:exposure
    bind:baseDomain
    {selectableDomains}
    {ssoDomains}
    on:change={() => dispatch('change')}
  />
  {#if exposure.subdomain && appRecordDomains.has(baseDomain)}
    <p class="mt-4 p-3 rounded-lg border border-naslos-border bg-naslos-dark/40 text-xs text-gray-300">
      A CNAME <span class="font-mono">{exposure.subdomain}.{baseDomain}</span> will be created on
      this domain and removed when the app is uninstalled or the subdomain changes.
    </p>
  {/if}
{/if}
