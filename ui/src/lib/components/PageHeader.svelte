<script lang="ts">
  // Shared page header: keeps the h1 accessible name and the title/subtitle
  // actions layout identical across routes.
  export let title = '';
  export let subtitle = '';
</script>

<div class="flex flex-wrap items-start justify-between gap-3 mb-8">
  <div class="min-w-0">
    <h1 class="text-3xl font-bold mb-2">{title}</h1>
    {#if subtitle}
      <p class="text-gray-400">{subtitle}</p>
    {/if}
  </div>
  {#if $$slots.actions}
    <div class="flex flex-wrap items-center gap-2 shrink-0">
      <slot name="actions" />
    </div>
  {/if}
</div>
