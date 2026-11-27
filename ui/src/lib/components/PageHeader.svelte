<script lang="ts">
  // Shared page header: one h1 per route, an optional breadcrumb trail, and a
  // wrapping actions area. It also owns the document title so every page that
  // uses it gets `Title · Naslos` without repeating a <svelte:head> block.
  interface Crumb {
    label: string;
    href?: string;
  }

  export let title = '';
  export let subtitle = '';
  export let breadcrumbs: Crumb[] = [];
</script>

<svelte:head>
  <title>{title ? `${title} · Naslos` : 'Naslos'}</title>
</svelte:head>

<div class="mb-8">
  {#if breadcrumbs.length > 0}
    <nav aria-label="Breadcrumb" class="mb-2">
      <ol class="flex flex-wrap items-center gap-1.5 text-sm text-gray-500">
        {#each breadcrumbs as crumb, i}
          <li class="flex items-center gap-1.5">
            {#if crumb.href && i < breadcrumbs.length - 1}
              <a class="text-naslos-link hover:underline" href={crumb.href}>{crumb.label}</a>
            {:else}
              <span aria-current={i === breadcrumbs.length - 1 ? 'page' : undefined}>{crumb.label}</span>
            {/if}
            {#if i < breadcrumbs.length - 1}<span aria-hidden="true">/</span>{/if}
          </li>
        {/each}
      </ol>
    </nav>
  {/if}

  <div class="flex flex-wrap items-start justify-between gap-3">
    <div class="min-w-0 flex-1 basis-64">
      <h1 class="text-3xl font-bold mb-2">{title}</h1>
      {#if subtitle}
        <p class="text-gray-400">{subtitle}</p>
      {/if}
    </div>
    {#if $$slots.actions}
      <div class="flex shrink-0 flex-wrap items-center gap-2">
        <slot name="actions" />
      </div>
    {/if}
  </div>
</div>
