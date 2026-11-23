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
  // Safe config keys the API redacts from responses (for example a generic
  // provider's updateUrl/body). Their stored values cannot be prefilled.
  export let redactedKeys: string[] = [];
  // Explicit "clear this stored value" choices, mutated in place. A blank
  // input alone never clears a write-only value.
  export let cleared: Record<string, boolean> = {};

  $: secretConfigured = new Set(secretSet);
  $: redactedConfigured = new Set(redactedKeys);
</script>

<div class="contents" on:change>
  {#each fields as f (f.key)}
    {@const configured = secretConfigured.has(f.key) || redactedConfigured.has(f.key)}
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
      {:else if f.type === 'bool'}
        <label class="flex items-center gap-2 text-sm text-gray-300 cursor-pointer">
          <input
            id={`${idPrefix}-field-${f.key}`}
            type="checkbox"
            class="w-4 h-4 rounded"
            checked={values[f.key] === 'true'}
            on:change={(e) => (values[f.key] = e.currentTarget.checked ? 'true' : 'false')}
          />
          <span>{values[f.key] === 'true' ? 'Enabled' : 'Disabled'}</span>
        </label>
        {#if f.key === 'allowInsecureHttp'}
          <p class="text-xs text-amber-300 mt-1">
            Only enable this for a plaintext <span class="font-mono">http</span> target you trust:
            credentials and update traffic travel unencrypted. The acknowledgement is audited.
          </p>
        {/if}
      {:else if f.secret}
        <input
          id={`${idPrefix}-field-${f.key}`}
          class="input"
          type="password"
          autocomplete="new-password"
          bind:value={values[f.key]}
          placeholder={secretConfigured.has(f.key) ? unchangedPlaceholder : ''}
        />
      {:else}
        <input
          id={`${idPrefix}-field-${f.key}`}
          class="input"
          bind:value={values[f.key]}
          placeholder={redactedConfigured.has(f.key) ? 'Configured — leave blank to keep' : ''}
        />
      {/if}
      {#if configured && !(redactedConfigured.has(f.key) && f.required)}
        <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
          <input
            id={`${idPrefix}-clear-${f.key}`}
            type="checkbox"
            class="w-4 h-4 rounded"
            bind:checked={cleared[f.key]}
          />
          Clear stored value
        </label>
      {/if}
    </div>
  {/each}
</div>
