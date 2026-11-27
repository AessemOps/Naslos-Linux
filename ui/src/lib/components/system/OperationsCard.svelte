<script lang="ts">
  import Button from '../Button.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import type { Op, Operation, Updates, VersionInfo } from './system';
  import { disabledReason, ops, opTitle } from './system';
  import { fmtTime } from '$lib/format';

  export let version: VersionInfo;
  export let updates: Updates | null = null;
  export let operation: Operation | null = null;
  export let reconnecting = false;
  export let onBegin: (op: Op) => void = () => {};

  function targetFor(op: Op): string {
    if (!updates?.available) return '';
    return op === 'naslos'
      ? updates.available.version
      : op === 'talos'
        ? updates.available.talosVersion
        : updates.available.kubernetesVersion;
  }
</script>

<section class="card mb-6" aria-labelledby="system-operations">
  <h2 id="system-operations" class="mb-4 text-lg font-bold">Upgrade operations</h2>
  <p class="mb-4 text-sm text-gray-400">
    The order is always Naslos, then Talos, then Kubernetes. Each step is pinned by the
    installed release.
  </p>
  <div class="space-y-3">
    {#each ops as op (op)}
      {@const reason = disabledReason(op, version, updates, operation)}
      <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg bg-naslos-dark p-3">
        <div>
          <p class="font-medium">{opTitle[op]}</p>
          {#if reason}
            <p class="mt-0.5 text-xs text-gray-500">{reason}</p>
          {:else if updates?.available}
            <p class="mt-0.5 text-xs text-gray-500">to {targetFor(op)}</p>
          {/if}
        </div>
        <Button variant="primary" disabled={!!reason} on:click={() => onBegin(op)}>
          {opTitle[op]}
        </Button>
      </div>
    {/each}
  </div>
</section>

{#if operation && operation.phase !== 'idle'}
  <section class="card" aria-labelledby="system-operation">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <h2 id="system-operation" class="text-lg font-bold">Operation {operation.op}</h2>
        <p class="text-sm text-gray-400">
          {operation.from} → {operation.to} · <span class="font-mono">{operation.phase}</span>
          {#if operation.finishedAt}· {fmtTime(operation.finishedAt)}{/if}
        </p>
      </div>
      {#if reconnecting}
        <p class="text-sm text-amber-300">node rebooting, reconnecting…</p>
      {/if}
    </div>
    {#if operation.message}
      <p class="mt-3 text-sm">{operation.message}</p>
    {/if}
    <ul class="mt-4 space-y-2">
      {#each operation.steps as step (step.name)}
        <li class="flex items-start gap-3 text-sm">
          <StatusBadge state={step.status} />
          <div>
            <p class="font-medium">{step.name}</p>
            <p class="text-gray-400">{step.message}</p>
          </div>
        </li>
      {/each}
    </ul>
    {#if operation.log}
      <pre class="mt-4 max-h-64 overflow-auto whitespace-pre-wrap rounded-lg bg-black/40 p-3 font-mono text-xs">{operation.log}</pre>
    {/if}
  </section>
{/if}
