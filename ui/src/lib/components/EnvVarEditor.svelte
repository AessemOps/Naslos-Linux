<script lang="ts">
  import type { EnvRow } from '$lib/appEnv';
  import { validEnvName } from '$lib/appEnv';

  export let env: EnvRow[] = [];
  /// Install mode requires a value for every row; an edit may leave a stored
  /// secret blank to keep the current value.
  export let requireValues = false;
  /// Rows whose value already exists in the Secret (placeholder hint).
  export let storedNames: Set<string> = new Set();

  function addRow() {
    env = [...env, { name: '', value: '', secret: false }];
  }

  function removeRow(index: number) {
    env = env.filter((_, i) => i !== index);
  }

  function update(index: number, patch: Partial<EnvRow>) {
    env = env.map((row, i) => (i === index ? { ...row, ...patch } : row));
  }

  $: names = env.map((row) => row.name.trim());
  $: duplicateNames = new Set(names.filter((name, i) => name && names.indexOf(name) !== i));
  $: invalidNames = new Set(names.filter((name) => name && !validEnvName(name)));
  $: missingValues = requireValues
    ? new Set(
        env
          .map((row, i) => ({ row, i }))
          .filter(({ row }) => row.value === null || row.value === '')
          .map(({ row }) => row.name.trim())
      )
    : new Set<string>();
</script>

<div class="space-y-2">
  {#each env as row, i (i)}
    <div class="flex items-start gap-2">
      <div class="flex-1">
        <input
          class="input w-full font-mono"
          class:border-red-600={invalidNames.has(row.name.trim()) || duplicateNames.has(row.name.trim())}
          placeholder="NAME"
          aria-label="Environment variable name"
          value={row.name}
          on:input={(e) => update(i, { name: e.currentTarget.value })}
        />
        {#if row.name.trim() && !validEnvName(row.name.trim())}
          <p class="text-xs text-red-400 mt-1">Use letters, digits, '_', '.' and '-', starting with a letter or '_'.</p>
        {:else if duplicateNames.has(row.name.trim())}
          <p class="text-xs text-red-400 mt-1">This name is used twice.</p>
        {/if}
      </div>
      <div class="flex-1">
        <input
          class="input w-full font-mono"
          type={row.secret ? 'password' : 'text'}
          placeholder={row.secret && row.value === null && storedNames.has(row.name.trim())
            ? 'unchanged (leave blank to keep)'
            : 'value'}
          aria-label="Environment variable value"
          value={row.value ?? ''}
          on:input={(e) => update(i, { value: e.currentTarget.value })}
        />
        {#if missingValues.has(row.name.trim())}
          <p class="text-xs text-red-400 mt-1">A value is required.</p>
        {/if}
      </div>
      <label class="flex items-center gap-1 text-xs text-gray-400 whitespace-nowrap pt-2">
        <input
          type="checkbox"
          checked={row.secret}
          on:change={(e) => update(i, { secret: e.currentTarget.checked })}
        />
        secret
      </label>
      <button
        type="button"
        class="text-gray-400 hover:text-red-300 text-lg leading-none pt-2"
        aria-label="Remove environment variable"
        on:click={() => removeRow(i)}>×</button
      >
    </div>
  {/each}
  <button type="button" class="btn btn-secondary text-sm" on:click={addRow}>+ Add variable</button>
  <p class="text-xs text-gray-500">
    Values are stored in a Kubernetes Secret and are never written to the app record or Helm values.
    Secret variables are masked in the UI; leave a stored secret blank to keep it.
    Names the chart itself sets (for example <span class="font-mono">TZ</span> or <span class="font-mono">PUID</span>)
    are rejected on save.
  </p>
</div>
