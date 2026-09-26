<script lang="ts">
  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    service?: string;
    port?: number;
    scheme?: string;
  }

  interface ServiceOption {
    name: string;
    port: number;
    scheme: string;
  }

  export let exposure: Exposure;
  export let baseDomain = '';
  export let selectableDomains: string[] = [];
  export let ssoDomains: string[] = [];
  export let services: ServiceOption[] = [];

  // Auth is only offered when the chosen domain is in the effective SSO list
  // (FR-APP-10). Derive it rather than reading it in a function called from the
  // template, which Svelte 5 would not track.
  $: authAllowed = ssoDomains.includes(baseDomain);

  function set<K extends keyof Exposure>(key: K, value: Exposure[K]) {
    exposure = { ...exposure, [key]: value };
  }

  function chooseTarget(value: string) {
    const svc = services.find((s) => `${s.name}:${s.port}` === value);
    if (svc) {
      exposure = { ...exposure, service: svc.name, port: svc.port, scheme: svc.scheme };
    }
  }

  // Changing the domain must clear auth when the new domain is not SSO-protected
  // (the server rejects auth on a non-SSO domain). Done in the handler, not a
  // reactive statement, so it never rewrites the caller's exposure on load.
  function chooseDomain(value: string) {
    baseDomain = value;
    if (!ssoDomains.includes(value)) {
      exposure = { ...exposure, auth: false };
    }
  }
</script>

<div class="space-y-5">
  <div>
    <label class="label" for="exposure-subdomain">Subdomain</label>
    <div class="flex items-center gap-2">
      <input
        id="exposure-subdomain"
        class="input flex-1"
        type="text"
        value={exposure.subdomain}
        placeholder="jellyfin"
        on:input={(e) => set('subdomain', (e.currentTarget as HTMLInputElement).value)}
      />
      {#if selectableDomains.length > 0}
        <span class="text-gray-400 text-sm">.</span>
        <select
          id="exposure-domain"
          class="input w-auto max-w-[14rem]"
          value={baseDomain}
          on:change={(e) => chooseDomain((e.currentTarget as HTMLSelectElement).value)}
        >
          {#each selectableDomains as domain}
            <option value={domain}>{domain}</option>
          {/each}
        </select>
      {:else}
        <span class="text-gray-400 text-sm whitespace-nowrap">.{baseDomain}</span>
      {/if}
    </div>
    <p class="text-xs text-gray-500 mt-1">
      Leave empty to keep the app cluster-internal (no route is created).
    </p>
  </div>

  {#if services.length > 0}
    <div>
      <label class="label" for="exposure-service">Route target</label>
      <select
        id="exposure-service"
        class="input"
        value={`${exposure.service ?? ''}:${exposure.port ?? ''}`}
        on:change={(e) => chooseTarget((e.currentTarget as HTMLSelectElement).value)}
      >
        {#if exposure.service}
          <option value={`${exposure.service}:${exposure.port ?? ''}`}>
            {exposure.service}:{exposure.port} ({exposure.scheme || 'http'}) — current
          </option>
        {/if}
        {#each services as svc}
          <option value={`${svc.name}:${svc.port}`}>{svc.name}:{svc.port} ({svc.scheme})</option>
        {/each}
      </select>
      <p class="text-xs text-gray-500 mt-1">
        The Service in the release this route forwards to.
      </p>
    </div>
  {:else if exposure.service}
    <div class="text-xs text-gray-500 border-t border-naslos-border pt-3">
      Routed to <span class="text-gray-300">{exposure.service}:{exposure.port}</span>
      ({exposure.scheme || 'http'}).
    </div>
  {/if}

  <label class="flex items-start gap-3 cursor-pointer">
    <input
      type="checkbox"
      class="mt-1"
      checked={exposure.tls}
      on:change={(e) => set('tls', (e.currentTarget as HTMLInputElement).checked)}
    />
    <span>
      <span class="font-medium">TLS</span>
      <span class="block text-xs text-gray-500">
        Serve over HTTPS using the domain's certificate Secret.
      </span>
    </span>
  </label>

  <label class={`flex items-start gap-3 ${authAllowed ? 'cursor-pointer' : 'opacity-60'}`}>
    <input
      type="checkbox"
      class="mt-1"
      checked={exposure.auth}
      disabled={!authAllowed}
      on:change={(e) => set('auth', (e.currentTarget as HTMLInputElement).checked)}
    />
    <span>
      <span class="font-medium">Require Authelia login</span>
      <span class="block text-xs {authAllowed ? 'text-gray-500' : 'text-amber-400'}">
        {authAllowed
          ? 'Protect this app with single sign-on.'
          : `Unavailable: ${baseDomain} is not in the SSO domain list.`}
      </span>
    </span>
  </label>

  <label class="flex items-start gap-3 cursor-pointer">
    <input
      type="checkbox"
      class="mt-1"
      checked={exposure.localOnly}
      on:change={(e) => set('localOnly', (e.currentTarget as HTMLInputElement).checked)}
    />
    <span>
      <span class="font-medium">Local network only</span>
      <span class="block text-xs text-gray-500">
        Allow only clients from the configured LAN CIDR.
      </span>
    </span>
  </label>
</div>
