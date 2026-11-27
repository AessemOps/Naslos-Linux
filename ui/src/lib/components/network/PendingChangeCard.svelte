<script lang="ts">
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import type { Pending } from './network';
  import { phaseLabel } from './network';

  export let pending: Pending;
  export let pendingSeconds = 0;
  export let saving = false;
  export let onRevert: () => void = () => {};
</script>

<section class="card mb-6 border-amber-700" aria-labelledby="network-pending">
  <div class="flex flex-wrap items-start justify-between gap-3">
    <div class="flex min-w-0 items-start gap-3">
      <div class="mt-0.5 shrink-0 text-amber-300" aria-hidden="true">
        <Icon name="refresh-cw" />
      </div>
      <div class="min-w-0">
        <h2 id="network-pending" class="font-bold text-amber-300">{phaseLabel(pending.phase)}</h2>
        <div class="text-sm text-gray-400">
          <span class="font-mono">{pending.link}</span>
          {#if pending.target}→ <span class="font-mono">{pending.target}</span>{/if}
          {#if pendingSeconds > 0}· {pendingSeconds}s remaining{/if}
        </div>
        {#if pending.error}
          <div class="mt-1 text-sm text-red-300">{pending.error}</div>
        {/if}
      </div>
    </div>
    <Button variant="danger" loading={saving} on:click={onRevert}>Revert now</Button>
  </div>
  <p class="mt-3 text-xs text-gray-500">
    A second change is blocked until this one finishes. A bad edit is reverted automatically by
    the node after 60 seconds if the new address never answers.
  </p>
</section>
