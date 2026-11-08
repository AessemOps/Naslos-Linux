<script lang="ts">
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
    values[key] = value;
    values = values;
  }

  function subEntries(prop: any): [string, any][] {
    return Object.entries(prop?.properties ?? {});
  }

  $: entries = Object.entries(app.schema.properties as Record<string, any>);
</script>

<div class="space-y-4">
  {#each entries as [key, prop]}
    <div>
      <label class="label" for="field-{key}">
        {prop.title || key}
        {#if app.schema.required?.includes(key)}<span class="text-red-400">*</span>{/if}
      </label>

      {#if prop.type === 'boolean'}
        <label class="flex items-center gap-3 cursor-pointer">
          <input
            type="checkbox"
            id="field-{key}"
            checked={values[key] ?? prop.default ?? false}
            on:change={(e) => values[key] = e.currentTarget.checked}
            class="w-5 h-5 rounded"
          />
          <span class="text-gray-300">{prop.description || 'Enable'}</span>
        </label>
      {:else if prop.type === 'object' && prop.properties}
        <div class="border border-naslos-border rounded-lg p-4 space-y-3">
          {#if prop.description}<p class="text-sm text-gray-400 mb-2">{prop.description}</p>{/if}
          {#each subEntries(prop) as [subKey, subProp]}
            <div>
              <label class="label" for="field-{key}-{subKey}">{subProp.title || subKey}</label>
              {#if subProp.type === 'boolean'}
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
              {:else}
                <input
                  type={inputType(subProp)}
                  id="field-{key}-{subKey}"
                  value={values[key]?.[subKey] ?? ''}
                  on:input={(e) => updateNested(values, key, subKey, coerce(subProp, e.currentTarget.value))}
                  placeholder={subProp.default || subProp.description || ''}
                  class="input w-full"
                />
              {/if}
            </div>
          {/each}
        </div>
      {:else if isGroupField(key, prop)}
        <select
          id="field-{key}"
          class="input w-full"
          value={String(currentGroup(key, prop))}
          on:change={(e) => updateValue(key, Number(e.currentTarget.value))}
        >
          {#each groupOptions as option}
            <option value={String(option.value)}>{option.label}</option>
          {/each}
          {#if !groupOptions.some((option) => option.value === currentGroup(key, prop))}
            <option value={String(currentGroup(key, prop))}>custom ({currentGroup(key, prop)})</option>
          {/if}
        </select>
        <p class="text-xs text-gray-500 mt-1">
          Datasets are owned by group 10000 (naslos_users); an app that mounts datasets must run with that group.
        </p>
      {:else}
        <input
          type={inputType(prop)}
          id="field-{key}"
          value={values[key] ?? ''}
          on:input={(e) => updateValue(key, coerce(prop, e.currentTarget.value))}
          placeholder={placeholderFor(key, prop)}
          class="input w-full"
        />
      {/if}

      {#if prop.description && prop.type !== 'object'}
        <p class="text-xs text-gray-500 mt-1">{prop.description}</p>
      {/if}
    </div>
  {/each}
</div>
