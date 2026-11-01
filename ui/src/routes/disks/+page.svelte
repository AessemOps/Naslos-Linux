<script lang="ts">
  import DiskWizard from '$lib/components/DiskWizard.svelte';
</script>

<div class="max-w-4xl mx-auto">
  <div class="flex flex-wrap items-start justify-between gap-3 mb-8">
    <div>
      <h1 class="text-3xl font-bold mb-2">Disk Setup</h1>
      <p class="text-gray-400">Create and manage ZFS pools with best-practice recommendations.</p>
    </div>
    <a href="/disks/health" class="px-4 py-2 rounded-lg border border-naslos-border hover:border-naslos-accent text-sm text-gray-300">
      View disk health →
    </a>
  </div>
  <DiskWizard />
</div>
