<script lang="ts">
  import FormField from './FormField.svelte';

  export let app: {
    schema: { properties: Record<string, any>; required?: string[] };
  };
  export let values: Record<string, any>;
  /// Secret values the API masked on read. Leaving the field untouched keeps
  /// the stored value (the request sends `null` and the server preserves it).
  export let redactedKeys: string[] = [];
  /// Suggestions for the numeric `pgid` field. Datasets on a Naslos node are
  /// group-owned by `naslos_users` (10000), so an app that mounts them must run
  /// with that group.
  export let groupOptions: { value: number; label: string }[] = [];

  function placeholderFor(key: string, prop: any): string {
    if (redactedKeys.includes(key) && (values[key] ?? null) === null) {
      return 'unchanged (leave blank to keep the current value)';
    }
    return prop.default || prop.description || '';
  }

  function inputType(prop: any): string {
    if (prop.format === 'password') return 'password';
    if (prop.format === 'email') return 'email';
    if (prop.type === 'integer' || prop.type === 'number' || prop.format === 'number') return 'number';
    return 'text';
  }

  /// JSON schema integer/number fields must reach the API as numbers; an
  /// `<input>` always yields a string.
  function coerce(prop: any, raw: string): any {
    if (prop.type === 'integer') {
      const parsed = Number.parseInt(raw, 10);
      return Number.isNaN(parsed) ? raw : parsed;
    }
    if (prop.type === 'number') {
      const parsed = Number(raw);
      return Number.isNaN(parsed) ? raw : parsed;
    }
    return raw;
  }

  function isGroupField(key: string, prop: any): boolean {
    return key === 'pgid' && prop.type === 'integer' && groupOptions.length > 0;
  }

  function currentGroup(key: string, prop: any): number {
    return Number(values[key] ?? prop.default ?? 0);
  }

  function updateNested(obj: Record<string, any>, key: string, subKey: string, value: any) {
    if (!obj[key]) obj[key] = {};
    obj[key][subKey] = value;
    values = values;
  }

  function updateValue(key: string, value: any) {
    values = { ...values, [key]: value };
  }

  function subEntries(prop: any): [string, any][] {
    return Object.entries(prop?.properties ?? {});
  }

  $: entries = Object.entries(app.schema.properties as Record<string, any>);
</script>

<div class="space-y-4">
  {#each entries as [key, prop]}
    {@const required = app.schema.required?.includes(key) ?? false}
    {#if prop.type === 'boolean'}
      <FormField label={prop.title || key} id="field-{key}" {required}>
        <label class="flex items-center gap-3 cursor-pointer">
          <input
            type="checkbox"
            id="field-{key}"
            checked={values[key] ?? prop.default ?? false}
            on:change={(e) => updateValue(key, e.currentTarget.checked)}
            class="w-5 h-5 rounded"
          />
          <span class="text-gray-300">{prop.description || 'Enable'}</span>
        </label>
      </FormField>
    {:else if prop.type === 'object' && prop.properties}
      <div class="border border-naslos-border rounded-lg p-4 space-y-3">
        <h3 class="text-sm font-semibold text-gray-300">{prop.title || key}</h3>
        {#if prop.description}<p class="text-sm text-gray-400">{prop.description}</p>{/if}
        {#each subEntries(prop) as [subKey, subProp]}
          {#if subProp.type === 'boolean'}
            <FormField label={subProp.title || subKey} id="field-{key}-{subKey}">
              <label class="flex items-center gap-3 cursor-pointer">
                <input
                  type="checkbox"
                  id="field-{key}-{subKey}"
                  checked={values[key]?.[subKey] ?? subProp.default ?? false}
                  on:change={(e) => updateNested(values, key, subKey, e.currentTarget.checked)}
                  class="w-5 h-5 rounded"
                />
                <span class="text-gray-300">{subProp.description || 'Enable'}</span>
              </label>
            </FormField>
          {:else}
            <FormField
              label={subProp.title || subKey}
              id="field-{key}-{subKey}"
              hint={subProp.description || ''}
            >
              <input
                type={inputType(subProp)}
                id="field-{key}-{subKey}"
                value={values[key]?.[subKey] ?? ''}
                on:input={(e) => updateNested(values, key, subKey, coerce(subProp, e.currentTarget.value))}
                placeholder={subProp.default || subProp.description || ''}
                class="input w-full"
              />
            </FormField>
          {/if}
        {/each}
      </div>
    {:else if isGroupField(key, prop)}
      <FormField
        label={prop.title || key}
        id="field-{key}"
        {required}
        hint="Datasets are owned by group 10000 (naslos_users); an app that mounts datasets must run with that group."
      >
        <select
          id="field-{key}"
          class="input w-full"
          value={String(Number(values[key] ?? prop.default ?? 0))}
          on:change={(e) => updateValue(key, Number(e.currentTarget.value))}
        >
          {#each groupOptions as option}
            <option value={String(option.value)}>{option.label}</option>
          {/each}
          {#if !groupOptions.some((option) => option.value === currentGroup(key, prop))}
            <option value={String(Number(values[key] ?? prop.default ?? 0))}>custom ({currentGroup(key, prop)})</option>
          {/if}
        </select>
      </FormField>
    {:else}
      <FormField label={prop.title || key} id="field-{key}" {required}>
        <input
          type={inputType(prop)}
          id="field-{key}"
          value={values[key] ?? ''}
          on:input={(e) => updateValue(key, coerce(prop, e.currentTarget.value))}
          placeholder={placeholderFor(key, prop)}
          class="input w-full"
        />
      </FormField>
    {/if}
    {#if prop.description && prop.type !== 'boolean' && prop.type !== 'object' && !isGroupField(key, prop)}
      <p class="text-xs text-gray-500">{prop.description}</p>
    {/if}
  {/each}
</div>
