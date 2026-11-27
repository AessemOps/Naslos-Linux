<script lang="ts">
  // Inline result for one channel, rendered next to that channel's actions so a
  // save/test outcome can never appear under a different channel.
  export let message = '';
  export let type: 'success' | 'error' | 'info' = 'success';
</script>

{#if message}
  <div
    class={`callout ${type === 'error' ? 'callout-bad' : type === 'success' ? 'callout-ok' : 'callout-info'}`}
    role={type === 'error' ? 'alert' : 'status'}
  >
    <span>{message}</span>
  </div>
{/if}
