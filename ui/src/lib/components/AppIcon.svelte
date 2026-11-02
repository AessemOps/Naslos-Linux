<script lang="ts">
  export let icon = '';
  export let iconUrl = '';
  export let alt = '';
  export let size = 'text-4xl';

  let failed = false;

  $: showImage = !!iconUrl && /^https:\/\//i.test(iconUrl) && !failed;
</script>

{#if showImage}
  <img
    src={iconUrl}
    {alt}
    class="h-10 w-10 shrink-0 rounded object-contain"
    loading="lazy"
    on:error={() => (failed = true)}
  />
{:else}
  <div class={`${size} shrink-0`} aria-hidden="true">{icon || '📦'}</div>
{/if}
