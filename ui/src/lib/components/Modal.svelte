<script lang="ts">
  // Modal dialog wrapper. The a11y action (role=dialog, aria-modal, labelled
  // by the heading, focus trap, Escape, scroll lock, inert background, focus
  // restoration) is shared with every caller via use:modal; this component adds
  // the backdrop-click close and a 40px close button so callers cannot forget
  // either path.
  //
  // Existing callers render the component conditionally and pass `wide`;
  // `wide` is kept as an alias for size="lg". New code should pass `size`.
  import { modal } from '$lib/actions/modal';
  import Icon from './Icon.svelte';

  export let open = true;
  export let title = '';
  export let onClose: () => void = () => {};
  export let size: 'sm' | 'md' | 'lg' | 'xl' | '' = '';
  export let wide = false;

  const widths: Record<'sm' | 'md' | 'lg' | 'xl', string> = {
    sm: 'max-w-sm',
    md: 'max-w-lg',
    lg: 'max-w-2xl',
    xl: 'max-w-4xl'
  };

  // Close on a backdrop click only when the press also started on the
  // backdrop: a text-selection drag that begins in the panel and ends over the
  // overlay dispatches `click` on the overlay and would discard form input.
  let pressedOnBackdrop = false;
  function onBackdropDown(event: MouseEvent) {
    pressedOnBackdrop = event.target === event.currentTarget;
  }
  function onBackdropClick(event: MouseEvent) {
    if (pressedOnBackdrop && event.target === event.currentTarget) onClose();
    pressedOnBackdrop = false;
  }

  $: resolvedSize = (size || (wide ? 'lg' : 'md')) as 'sm' | 'md' | 'lg' | 'xl';
</script>

{#if open}
  <!-- The overlay keeps the .fixed.inset-0 class the e2e suite selects. -->
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-0 sm:p-4"
    use:modal={{ onClose }}
    on:mousedown={onBackdropDown}
    on:click={onBackdropClick}
  >
    <div
      class="flex h-full max-h-full w-full flex-col overflow-hidden border border-naslos-border bg-naslos-surface shadow-xl sm:h-auto sm:rounded-2xl {widths[
        resolvedSize
      ]} sm:max-h-[85vh]"
    >
      <div class="flex items-center justify-between gap-3 border-b border-naslos-border p-4 sm:p-6">
        <h2 class="text-xl font-bold">{title}</h2>
        <button
          type="button"
          class="icon-btn -mr-2 text-gray-400 transition-colors hover:bg-naslos-border hover:text-naslos-strong"
          aria-label="Close"
          on:click={onClose}
        >
          <Icon name="x" />
        </button>
      </div>

      <div class="flex-1 space-y-4 overflow-y-auto p-4 sm:p-6">
        <slot />
      </div>

      {#if $$slots.footer}
        <div
          class="flex flex-col-reverse gap-2 border-t border-naslos-border p-4 [&>*]:w-full sm:flex-row sm:justify-end sm:p-6 sm:[&>*]:w-auto"
        >
          <slot name="footer" />
        </div>
      {/if}
    </div>
  </div>
{/if}
