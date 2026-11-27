<script lang="ts">
  import Button from '../Button.svelte';

  export let rebooting = false;
  export let operationActive = false;
  export let onReboot: () => void = () => {};
</script>

<section class="card mb-6" aria-labelledby="system-node">
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div>
      <h2 id="system-node" class="text-lg font-bold">Node</h2>
      <p class="mt-1 text-sm text-gray-400">
        Reboot the Talos node. Shares and apps go offline until it returns.
      </p>
      {#if rebooting}
        <p class="mt-2 text-sm text-amber-300">node rebooting, reconnecting...</p>
      {:else if operationActive}
        <p class="mt-2 text-sm text-amber-300">an operation is running</p>
      {/if}
    </div>
    <Button
      variant="danger"
      loading={rebooting}
      disabled={rebooting || operationActive}
      on:click={onReboot}
    >
      {rebooting ? 'Rebooting...' : 'Reboot node'}
    </Button>
  </div>
</section>
