<script lang="ts">
  // Shared event checkbox list. The parent binds `selected`; toggling assigns a
  // new array so the parent's draft updates without mutating the loaded GET
  // object.
  import type { EventType } from './types';
  import { toggleId } from './types';

  export let events: EventType[] = [];
  export let selected: string[] = [];
  export let disabled = false;

  function toggle(id: string) {
    selected = toggleId(selected, id);
  }
</script>

<div class="space-y-1">
  {#each events as event (event.id)}
    <label
      class="flex cursor-pointer items-center gap-3 rounded p-2 hover:bg-naslos-dark"
      class:opacity-60={disabled}
    >
      <input
        type="checkbox"
        class="h-4 w-4 rounded"
        checked={selected.includes(event.id)}
        {disabled}
        on:change={() => toggle(event.id)}
      />
      <span class="text-gray-300">{event.label}</span>
    </label>
  {/each}
</div>
