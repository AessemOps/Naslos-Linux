<script lang="ts">
  export let value = '';
  export let source = '';
</script>

<section class="card mb-6" aria-labelledby="network-hostname">
  <h2 id="network-hostname" class="mb-3 text-lg font-bold">Hostname</h2>
  <dl class="grid gap-2 text-sm sm:grid-cols-2">
    <div>
      <dt class="text-gray-400">Hostname</dt>
      <dd class="font-mono">{value || '—'}</dd>
    </div>
    <div>
      <dt class="text-gray-400">Source</dt>
      <dd>{source}</dd>
    </div>
  </dl>
  <p class="mt-3 text-xs text-gray-500">
    The install path uses the stable auto hostname. Renaming the node is not offered here: it
    moves every local-path volume's node affinity and would strand installed apps.
  </p>
</section>
