<script lang="ts">
  import type { VersionInfo } from './system';

  export let version: VersionInfo;
</script>

<section class="card mb-6" aria-labelledby="system-versions">
  <h2 id="system-versions" class="mb-4 text-lg font-bold">Versions</h2>
  <dl class="grid gap-3 sm:grid-cols-2">
    <div class="flex items-baseline justify-between gap-4 rounded-lg bg-naslos-dark px-3 py-2">
      <dt class="text-gray-400">Naslos</dt>
      <dd class="font-mono">{version.naslosVersion || '—'}</dd>
    </div>
    <div class="flex items-baseline justify-between gap-4 rounded-lg bg-naslos-dark px-3 py-2">
      <dt class="text-gray-400">Chart</dt>
      <dd class="font-mono">{version.chartVersion || '—'}</dd>
    </div>
    <div class="flex items-baseline justify-between gap-4 rounded-lg bg-naslos-dark px-3 py-2">
      <dt class="text-gray-400">Talos</dt>
      <dd class="font-mono">{version.talosVersion || '—'}</dd>
    </div>
    <div class="flex items-baseline justify-between gap-4 rounded-lg bg-naslos-dark px-3 py-2">
      <dt class="text-gray-400">Kubernetes</dt>
      <dd class="font-mono">{version.kubernetesVersion || '—'}</dd>
    </div>
  </dl>
  {#if !version.upgradable}
    <p class="mt-4 text-sm text-amber-300">
      {#if version.profile === 'installer'}
        The installer release facts are incomplete (Naslos, Talos or
        Kubernetes version missing), so web upgrades are disabled. Reinstall
        from a current pack, or use <span class="font-mono">make install-vm</span>.
      {:else}
        This is a <span class="font-mono">make install-vm</span> deployment.
        It reports its versions here, but web upgrades are disabled; apply
        releases with <span class="font-mono">make install-vm</span>. An
        installer node can use the web upgrader after bootstrapping with the
        0.3.0 installer <span class="font-mono">--resume</span>.
      {/if}
    </p>
  {/if}
</section>
