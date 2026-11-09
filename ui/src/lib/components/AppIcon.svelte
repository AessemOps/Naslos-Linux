<script lang="ts">
  export let icon = '';
  export let iconUrl = '';
  export let alt = '';
  export let size = 'text-4xl';
  /// Tailwind size classes for the image variant (rows use a smaller icon).
  export let imgClass = 'h-10 w-10';

  let failed = false;

  $: showImage = !!iconUrl && /^https:\/\//i.test(iconUrl) && !failed;
</script>

{#if showImage}
  <img
    src={iconUrl}
    {alt}
    class={`${imgClass} shrink-0 rounded object-contain`}
    loading="lazy"
    on:error={() => (failed = true)}
  />
{:else}
  <div class={`${size} shrink-0`} aria-hidden="true">{icon || '📦'}</div>
{/if}
