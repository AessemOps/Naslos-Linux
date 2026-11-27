<script lang="ts">
  // One KPI card on the dashboard: label, large value with its unit, a detail
  // line and a small trend line. The value keeps the `.text-3xl` class the
  // dashboard spec asserts; the unit sits in the same element so the rendered
  // text stays a single "42.0%" string.
  import Sparkline from '../Sparkline.svelte';

  export let label = '';
  export let value = '—';
  export let unit = '';
  export let tone: 'ok' | 'warn' | 'bad' | 'neutral' = 'neutral';
  export let detail = '';
  export let values: number[] = [];

  $: toneText =
    tone === 'bad'
      ? 'text-red-300'
      : tone === 'warn'
        ? 'text-amber-300'
        : tone === 'ok'
          ? 'text-green-400'
          : 'text-naslos-link';
</script>

<div class="card">
  <div class="text-sm text-gray-400">{label}</div>
  <div class="mt-1 text-3xl font-bold {toneText}">{value}{#if unit}<span class="ml-1 text-base font-medium text-gray-400">{unit}</span>{/if}</div>
  {#if detail}
    <div class="text-xs text-gray-500">{detail}</div>
  {/if}
  <slot />
  <div class="mt-3" aria-hidden="true">
    <Sparkline {values} height={36} colorClass={toneText} />
  </div>
</div>
