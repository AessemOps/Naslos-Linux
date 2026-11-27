<script lang="ts">
  // Shared button. Keeps the existing .btn/.btn-* visual classes, adds a typed
  // API, a loading state and link rendering.
  //
  //   <Button variant="danger" on:click={remove}>Delete</Button>
  //   <Button href="/pools" variant="secondary">Pools</Button>
  import { createEventDispatcher } from 'svelte';

  export let variant: 'primary' | 'secondary' | 'danger' | 'ghost' = 'primary';
  export let size: 'sm' | 'md' | 'icon' = 'md';
  export let type: 'button' | 'submit' | 'reset' = 'button';
  export let loading = false;
  export let disabled = false;
  export let href = '';

  const dispatch = createEventDispatcher<{ click: MouseEvent }>();

  $: variantClass = `btn-${variant}`;
  $: classes = [
    size === 'icon' ? 'icon-btn' : 'btn',
    size === 'sm' ? 'btn-sm' : '',
    variantClass
  ]
    .filter(Boolean)
    .join(' ');
  $: inactive = disabled || loading;

  function onClick(event: MouseEvent) {
    if (inactive) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    dispatch('click', event);
  }
</script>

{#if href}
  <a
    {...$$restProps}
    {href}
    class={classes}
    aria-disabled={inactive ? 'true' : undefined}
    aria-busy={loading ? 'true' : undefined}
    on:click={onClick}
  >
    {#if loading}
      <span
        class="h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent"
        aria-hidden="true"
      ></span>
    {/if}
    <slot />
  </a>
{:else}
  <button
    {...$$restProps}
    class={classes}
    {type}
    disabled={inactive}
    aria-busy={loading ? 'true' : undefined}
    on:click={onClick}
  >
    {#if loading}
      <span
        class="h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent"
        aria-hidden="true"
      ></span>
    {/if}
    <slot />
  </button>
{/if}
