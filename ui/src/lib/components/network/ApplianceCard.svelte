<script lang="ts">
  export let domain = '';
  export let lanCidr = '';
  export let primaryLink = '';
</script>

<section class="card" aria-labelledby="network-appliance">
  <h2 id="network-appliance" class="mb-3 text-lg font-bold">Appliance</h2>
  <dl class="grid gap-2 text-sm sm:grid-cols-2">
    <div>
      <dt class="text-gray-400">Base domain</dt>
      <dd class="font-mono">{domain || 'not configured'}</dd>
    </div>
    <div>
      <dt class="text-gray-400">LAN CIDR</dt>
      <dd class="font-mono">{lanCidr || 'not configured'}</dd>
    </div>
    <div>
      <dt class="text-gray-400">Primary link</dt>
      <dd class="font-mono">{primaryLink || 'unknown'}</dd>
    </div>
  </dl>
  <p class="mt-3 text-sm text-gray-400">
    Domain and certificate settings live in <a class="underline" href="/domains">Domains</a>;
    public records in <a class="underline" href="/dns">Dynamic DNS</a>.
  </p>
</section>
