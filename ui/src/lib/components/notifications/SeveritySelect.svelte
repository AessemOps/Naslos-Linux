<script lang="ts">
  import FormField from '../FormField.svelte';
  import type { Choice } from './types';
  import { severityLevels } from './types';

  export let id = '';
  export let value = 'warning';
  export let label = 'Minimum Severity';
  export let ariaLabel = '';
  export let levels: Choice[] = severityLevels;
</script>

<FormField {label} {id}>
  <select {id} class="input w-full" bind:value aria-label={ariaLabel || undefined}>
    {#each levels as level (level.id)}
      <option value={level.id}>{level.label}</option>
    {/each}
  </select>
</FormField>
