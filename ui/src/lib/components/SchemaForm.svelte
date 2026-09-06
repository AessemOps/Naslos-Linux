<script lang="ts">
  export let app: {
    schema: { properties: Record<string, any>; required?: string[] };
  };
  export let values: Record<string, any>;

  function inputType(format?: string): string {
    if (format === 'password') return 'password';
    if (format === 'email') return 'email';
    if (format === 'number') return 'number';
    return 'text';
  }

  function updateNested(obj: Record<string, any>, key: string, subKey: string, value: any) {
    if (!obj[key]) obj[key] = {};
    obj[key][subKey] = value;
  }

  $: entries = Object.entries(app.schema.properties);
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
        <div class="border border-nasos-border rounded-lg p-4 space-y-3">
          {#if prop.description}<p class="text-sm text-gray-400 mb-2">{prop.description}</p>{/if}
          {#each Object.entries(prop.properties) as [subKey, subProp]}
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
                  type={inputType(subProp.format)}
                  id="field-{key}-{subKey}"
                  bind:value={values[key][subKey]}
                  placeholder={subProp.default || subProp.description || ''}
                  class="input w-full"
                />
              {/if}
            </div>
          {/each}
        </div>
      {:else}
        <input
          type={inputType(prop.format)}
          id="field-{key}"
          bind:value={values[key]}
          placeholder={prop.default || prop.description || ''}
          class="input w-full"
        />
      {/if}

      {#if prop.description && prop.type !== 'object'}
        <p class="text-xs text-gray-500 mt-1">{prop.description}</p>
      {/if}
    </div>
  {/each}
</div>
