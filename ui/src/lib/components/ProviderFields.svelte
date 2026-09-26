<script lang="ts">
  interface ProviderField {
    key: string;
    label: string;
    type: string;
    default?: string;
    enum?: string[];
    required?: boolean;
    secret?: boolean;
  }

  export let fields: ProviderField[] = [];
  // Mutated in place: the parent's object is the value store.
  export let values: Record<string, string> = {};
  export let idPrefix = 'provider';
  // Which secret fields are already stored (shows the "unchanged" hint).
  export let secretSet: string[] = [];
  export let unchangedPlaceholder = '';
</script>

<div class="contents" on:change>
  {#each fields as f (f.key)}
    <div>
      <label class="label" for={`${idPrefix}-field-${f.key}`}>
        {f.label || f.key}{#if f.required}<span class="text-red-400"> *</span>{/if}
      </label>
      {#if f.type === 'enum'}
        <select id={`${idPrefix}-field-${f.key}`} class="input" bind:value={values[f.key]}>
          {#each f.enum || [] as v (v)}
            <option value={v}>{v}</option>
          {/each}
        </select>
      {:else if f.secret}
        <input
          id={`${idPrefix}-field-${f.key}`}
          class="input"
          type="password"
          autocomplete="new-password"
          bind:value={values[f.key]}
          placeholder={secretSet.includes(f.key) ? unchangedPlaceholder : ''}
        />
      {:else}
        <input id={`${idPrefix}-field-${f.key}`} class="input" bind:value={values[f.key]} />
      {/if}
    </div>
  {/each}
</div>
