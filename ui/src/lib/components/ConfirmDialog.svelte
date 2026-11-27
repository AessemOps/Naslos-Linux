<script lang="ts">
  // Promise-based replacement for window.confirm(). Mounted once in
  // +layout.svelte; pages call confirmAction() and await the boolean.
  import { confirmState, resolveConfirm } from '$lib/stores/confirm';
  import Button from './Button.svelte';
  import FormField from './FormField.svelte';
  import Modal from './Modal.svelte';

  let typed = '';

  $: request = $confirmState;
  $: typedOk = !request?.typedConfirmation || typed === request.typedConfirmation;

  function close(result: boolean) {
    typed = '';
    resolveConfirm(result);
  }
</script>

{#if request}
  <Modal open={true} title={request.title} size="sm" onClose={() => close(false)}>
    <p class="whitespace-pre-line text-sm text-gray-300">{request.message}</p>
    {#if request.typedConfirmation}
      <FormField
        label={`Type ${request.typedConfirmation} to confirm`}
        id="confirm-typed"
        required
        hint="This action cannot be undone."
      >
        <input
          id="confirm-typed"
          class="input w-full"
          autocomplete="off"
          spellcheck="false"
          bind:value={typed}
        />
      </FormField>
    {/if}
    <svelte:fragment slot="footer">
      <Button variant="secondary" on:click={() => close(false)}>{request.cancelLabel}</Button>
      <Button
        variant={request.tone === 'danger' ? 'danger' : 'primary'}
        disabled={!typedOk}
        on:click={() => close(true)}
      >
        {request.confirmLabel}
      </Button>
    </svelte:fragment>
  </Modal>
{/if}
