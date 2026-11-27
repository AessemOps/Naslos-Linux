<script lang="ts">
  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    internet: boolean;
  }

  export let app: {
    source: string;
    chart: string;
    chartPath: string;
    privileged?: boolean;
  };
  export let exposure: Exposure;
  export let baseDomain = '';
  export let unsafeExposure = false;
  export let confirmed = false;
  export let unsafeConfirmed = false;
  export let privilegedConfirmed = false;
</script>

<div class="space-y-4">
  {#if unsafeExposure}
    <div class="callout callout-bad mb-0">
      <p class="font-medium mb-1">This app will be exposed without full protection</p>
      <p>
        {#if !exposure.auth}Without Naslos login (auth is off).{/if}
        {#if !exposure.tls}Without TLS (traffic is cleartext).{/if}
        Anything that can reach <span class="font-mono">{exposure.subdomain}.{baseDomain}</span>
        can use its admin interface.
      </p>
    </div>
  {/if}
  <div class="callout callout-warn mb-0">
    <p class="font-medium mb-1">Third-party chart trust boundary</p>
    <p>
      This installs <span class="font-mono">{app.source}</span> /
      <span class="font-mono">{app.chartPath || app.chart}</span> into
      <span class="font-mono">{app.privileged ? 'the privileged app pool' : 'an app namespace'}</span>. The
      chart's manifests run in-cluster under the API's namespaced permissions. Charts that request the
      datasets volume can read every Naslos dataset mounted below the platform's datasets root.
    </p>
  </div>
  {#if app.privileged}
    <div class="callout callout-bad mb-0">
      <p class="font-medium mb-1">This app requests privileged mode</p>
      <p>
        It will be installed into <span class="font-mono">naslos-apps-priv</span>,
        the namespace that allows privileged pods and host access. Only install
        it if you trust the chart source completely.
      </p>
    </div>
  {/if}
  <label class="flex items-start gap-3 cursor-pointer">
    <input type="checkbox" class="mt-1" bind:checked={confirmed} />
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
      <input type="checkbox" class="mt-1" bind:checked={privilegedConfirmed} />
      <span class="font-medium">I understand this app runs privileged and may access the host.</span>
    </label>
  {/if}
</div>
