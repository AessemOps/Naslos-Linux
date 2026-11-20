<script lang="ts">
  import Icon from './Icon.svelte';

  // Shared empty state. Copy that the Playwright suite asserts (e.g. the
  // dashboard's 'No ZFS pools configured yet.' + 'Create Your First Pool' link)
  // is passed in unchanged by the caller.
  export let icon = 'inbox';
  export let title = 'Nothing here yet';
  export let detail = '';
</script>

<div class="card text-center py-12">
  <div
    class="mx-auto mb-4 w-12 h-12 rounded-full bg-naslos-dark border border-naslos-border flex items-center justify-center text-gray-400"
    aria-hidden="true"
  >
    <Icon name={icon} size={24} />
  </div>
  <h2 class="text-xl font-bold mb-2">{title}</h2>
  {#if detail}
    <p class="text-gray-400 mb-4">{detail}</p>
  {:else}
    <div class="mb-4"></div>
  {/if}
  {#if $$slots.default}
    <div class="flex justify-center flex-wrap gap-3">
      <slot />
    </div>
  {/if}
</div>
