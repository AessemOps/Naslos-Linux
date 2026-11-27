<script lang="ts">
  import Button from '$lib/components/Button.svelte';
  import DiskWizard from '$lib/components/DiskWizard.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Disk Setup"
    subtitle="Create and manage ZFS pools with best-practice recommendations."
    breadcrumbs={[{ label: 'Storage' }, { label: 'Disks' }]}
  >
    <div slot="actions">
      <Button href="/disks/health" variant="secondary">View disk health</Button>
    </div>
  </PageHeader>

  <DiskWizard />
</div>
