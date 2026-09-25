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

  export let exposure: Exposure;
  export let baseDomain = '';
  export let authAllowed = true;

  function set<K extends keyof Exposure>(key: K, value: Exposure[K]) {
    exposure = { ...exposure, [key]: value };
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
      <span class="text-gray-400 text-sm whitespace-nowrap">.{baseDomain}</span>
    </div>
    <p class="text-xs text-gray-500 mt-1">
      Leave empty to keep the app cluster-internal (no route is created).
    </p>
  </div>

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

  {#if exposure.service}
    <div class="text-xs text-gray-500 border-t border-naslos-border pt-3">
      Routed to <span class="text-gray-300">{exposure.service}:{exposure.port}</span>
      ({exposure.scheme || 'http'}).
    </div>
  {/if}
</div>
