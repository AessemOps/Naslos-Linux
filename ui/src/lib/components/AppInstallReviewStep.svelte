<script lang="ts">
  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    internet: boolean;
  }

  export let app: {
    displayName: string;
    source: string;
    channel: string;
    chart: string;
    version: string;
    schema?: { properties?: Record<string, any> };
  };
  export let values: Record<string, any> = {};
  export let selectedDatasets: string[] = [];
  export let selectedPool = '';
  export let exposure: Exposure;
  export let baseDomain = '';
  export let envReview = '';
  /// Schema keys whose values are write-only (`format: password` / `writeOnly`).
  export let secretKeys = new Set<string>();

  // A review row must not print false/0 as an empty value; only null, undefined
  // and the empty string actually mean "not set".
  function displayValue(value: any): string {
    if (value === null || value === undefined || value === '') return '(empty)';
    return String(value);
  }

  function labelFor(key: string): string {
    const prop = app.schema?.properties?.[key];
    return prop?.title || key;
  }

  $: valueEntries = Object.entries(values).filter(([, value]) => typeof value !== 'object');
</script>

<div class="space-y-3">
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">App</span><span class="font-medium">{app.displayName}</span>
  </div>
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">Source</span><span class="font-medium text-sm">{app.source} / {app.channel}</span>
  </div>
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">Chart</span><span class="font-medium text-sm">{app.chart}</span>
  </div>
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">Version</span><span class="font-medium">{app.version}</span>
  </div>
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">Datasets</span>
    <span class="font-medium text-sm text-right">
      {selectedDatasets.length
        ? selectedDatasets.map((name) => `${name} → /datasets/${name}`).join(', ')
        : 'none'}
    </span>
  </div>
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">Storage pool</span>
    <span class="font-medium text-sm">{selectedPool || 'default (local-path)'}</span>
  </div>
  {#if envReview}
    <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
      <span class="text-gray-400">Environment</span>
      <span class="font-medium text-sm text-right font-mono">{envReview}</span>
    </div>
  {/if}
  <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
    <span class="text-gray-400">Exposure</span>
    <span class="font-medium text-sm text-right">
      {exposure.subdomain ? `${exposure.subdomain}.${baseDomain}` : 'cluster-internal only'}<br />
      TLS {exposure.tls ? 'on' : 'off'} · Auth {exposure.auth ? 'on' : 'off'} · Local-only {exposure.localOnly
        ? 'on'
        : 'off'} · Internet {exposure.internet ? 'on' : 'off'}
    </span>
  </div>
  {#each valueEntries as [key, value]}
    <div class="flex justify-between gap-4 py-2 border-b border-naslos-border">
      <span class="text-gray-400">{labelFor(key)}</span>
      <span class="font-medium text-sm text-right break-words"
        >{secretKeys.has(key) ? '••••••' : displayValue(value)}</span
      >
    </div>
  {/each}
</div>
