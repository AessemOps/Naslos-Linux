<script lang="ts">
  // Modal dialog wrapper. The a11y action (role=dialog, aria-modal, labelled
  // by the heading, focus trap, Escape, focus restoration) is shared with every
  // caller via use:modal; this component adds the backdrop-click close and a
  // 40px close button so callers cannot forget either path.
  import { modal } from '$lib/actions/modal';
  import Icon from './Icon.svelte';

  export let title = '';
  export let onClose: () => void = () => {};
  export let wide = false;

  // Wide dialogs (multi-column forms); default fits forms like New Dataset.
  const width = wide ? 'max-w-2xl' : 'max-w-lg';
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4"
  use:modal={{ onClose }}
  on:click|self={onClose}
>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full {width} max-h-[85vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between gap-3">
      <h2 class="text-xl font-bold">{title}</h2>
      <button
        type="button"
        class="icon-btn -mr-2 text-gray-400 hover:text-naslos-strong hover:bg-naslos-border"
        aria-label="Close"
        on:click={onClose}
      >
        <Icon name="x" />
      </button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <slot />
    </div>

    {#if $$slots.footer}
      <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
        <slot name="footer" />
      </div>
    {/if}
  </div>
</div>
