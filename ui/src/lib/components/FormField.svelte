<script lang="ts">
  // Label + hint + error wrapper for a single form control. The control stays
  // in the default slot so callers keep native <input>/<select>/<textarea>
  // bindings; pass `aria-describedby` yourself when you use hint/error ids.
  //
  //   <FormField label="Name" id="pool-name" required error={nameError}>
  //     <input id="pool-name" class="input w-full" bind:value={name} />
  //   </FormField>
  export let label = '';
  export let id = '';
  export let required = false;
  export let hint = '';
  export let error = '';
</script>

<div>
  <label class="label" for={id}>{label}{#if required} *{/if}</label>
  {#if hint}
    <p id="{id}-hint" class="mb-1 text-xs text-gray-500">{hint}</p>
  {/if}
  <slot />
  {#if error}
    <p id="{id}-error" role="alert" class="mt-1 text-xs text-red-300">{error}</p>
  {/if}
</div>
