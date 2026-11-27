<script lang="ts">
  import Icon from './Icon.svelte';

  // Shared empty state. Copy that the Playwright suite asserts (e.g. the
  // dashboard's 'No ZFS pools configured yet.' + 'Create Your First Pool' link)
  // is passed in unchanged by the caller. The named `action` slot is preferred
  // for new code; the default slot stays for existing call sites.
  export let icon = 'inbox';
  export let title = 'Nothing here yet';
  export let detail = '';
</script>

<div class="card py-12 text-center">
  <div
    class="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-naslos-border bg-naslos-dark text-gray-400"
    aria-hidden="true"
  >
    <Icon name={icon} size={24} />
  </div>
  <h2 class="mb-2 text-xl font-bold">{title}</h2>
  {#if detail}
    <p class="mb-4 text-gray-400">{detail}</p>
  {:else}
    <div class="mb-4"></div>
  {/if}
  {#if $$slots.action || $$slots.default}
    <div class="flex flex-wrap justify-center gap-3">
      {#if $$slots.action}
        <slot name="action" />
      {:else}
        <slot />
      {/if}
    </div>
  {/if}
</div>
