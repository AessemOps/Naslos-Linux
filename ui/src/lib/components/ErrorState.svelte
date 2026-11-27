<script lang="ts">
  import Icon from './Icon.svelte';

  // Shared error state for page-level fetch failures. The retry hook is
  // optional: pages that reload on navigation keep their simpler inline copy.
  export let message = '';
  export let retry: (() => void) | null = null;
</script>

<div
  role="alert"
  class="card mb-4 flex items-start justify-between gap-4 border-red-700 bg-red-900/20 p-4 text-red-300"
>
  <div class="flex min-w-0 items-start gap-3">
    <Icon name="triangle-alert" size={20} />
    <span class="break-words">{message}</span>
  </div>
  {#if retry}
    <button type="button" class="btn btn-sm btn-secondary shrink-0" on:click={retry}>Retry</button>
  {/if}
</div>
