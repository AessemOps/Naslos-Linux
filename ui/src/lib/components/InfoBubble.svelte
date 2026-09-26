<script context="module" lang="ts">
  let counter = 0;
</script>

<script lang="ts">
  export let items: string[] = [];
  export let label = 'API rights required';

  const id = `info-bubble-${++counter}`;
  let open = false;
  let pinned = false;

  function onFocus() {
    if (!pinned) open = true;
  }

  function onBlur() {
    pinned = false;
    open = false;
  }

  function onToggle() {
    pinned = !pinned;
    open = pinned;
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      pinned = false;
      open = false;
      (event.currentTarget as HTMLButtonElement).blur();
    }
  }
</script>

{#if items.length > 0}
  <span class="group relative inline-flex">
    <button
      type="button"
      class="inline-flex h-4 w-4 items-center justify-center rounded-full text-sm leading-none text-gray-400 hover:text-white focus:text-white focus:outline-none"
      aria-label={label}
      aria-expanded={open}
      aria-controls={id}
      on:click={onToggle}
      on:focus={onFocus}
      on:blur={onBlur}
      on:keydown={onKeydown}
    >
      <span aria-hidden="true">&#9432;</span>
    </button>
    <div
      id={id}
      role="tooltip"
      class="absolute left-0 top-full z-10 mt-2 w-72 rounded-lg border border-naslos-border bg-naslos-surface p-3 text-xs text-gray-300 shadow-lg before:absolute before:-top-2 before:left-0 before:h-2 before:w-full before:content-[''] {open
        ? 'block'
        : 'hidden'} group-hover:block"
    >
      <p class="mb-1 font-semibold text-gray-200">{label}</p>
      <ul class="list-disc space-y-1 pl-4">
        {#each items as item}
          <li>{item}</li>
        {/each}
      </ul>
    </div>
  </span>
{/if}
