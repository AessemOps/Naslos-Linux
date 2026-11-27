<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import ProviderFields from './ProviderFields.svelte';
  import { changedProviderFields, providerDefaults, visibleFields } from '$lib/providerFields';
  import type { DdnsEntryView, DnsProviderView } from '$lib/dnsTypes';

  export let entry: DdnsEntryView | null = null;
  export let providers: DnsProviderView[] = [];

  const ddnsProviders = providers.filter((p) => p.ddns);

  function initialProvider(): string {
    if (entry?.provider) return entry.provider;
    return ddnsProviders.length ? ddnsProviders[0].name : '';
  }

  let providerName = initialProvider();
  let zone = entry?.zone || '';
  let record = entry?.record || '@';
  let recordType = entry?.recordType || 'A';
  let ttl = entry?.ttl ?? 0;
  let enabled = entry ? !!entry.enabled : true;
  // Provider config keys the API redacts from the response (credentials and
  // free-form templates such as updateUrl/body). They are never prefilled.
  let redactedKeys: string[] = Array.isArray(entry?.redactedProviderConfigKeys)
    ? [...entry.redactedProviderConfigKeys]
    : [];

  // The provider the loaded credential fields belong to. After a provider
  // change the old Secret must not be presented as covering the new fields.
  const loadedProvider = entry?.provider ?? '';

  function fieldsFor(name: string) {
    return (ddnsProviders.find((p) => p.name === name)?.fields ?? []).filter(
      (f) => f.scope !== 'cert'
    );
  }

  function defaultsFor(name: string, seed: Record<string, string>): Record<string, string> {
    return providerDefaults(fieldsFor(name), seed, redactedKeys);
  }

  let fieldValues: Record<string, string> = defaultsFor(providerName, {
    ...(entry?.providerConfig || {})
  });
  let initialValues: Record<string, string> = { ...fieldValues };
  let clearedFields: Record<string, boolean> = {};
  let saving = false;
  let running = false;
  let error = '';
  let notice = '';

  // Svelte 5 does not track state read inside a function called from the
  // template, so derive the provider/fields reactively instead.
  $: currentProvider = ddnsProviders.find((p) => p.name === providerName);
  $: currentFields = currentProvider?.fields ?? [];
  // Hide certificate-only fields (scope) and fields whose showIf rule is not met
  // (e.g. OVH's ZoneDNS fields unless mode is api).
  $: ddnsFields = visibleFields(
    currentFields.filter((f) => f.scope !== 'cert'),
    fieldValues
  );
  $: providerChanged = !!entry && providerName !== loadedProvider;

  // Switch provider: keep the selection in sync and prefill field defaults.
  function onProviderChange(event: Event) {
    const nextName = (event.currentTarget as HTMLSelectElement).value;
    if (nextName === providerName) return;
    providerName = nextName;
    // A different provider starts from its own defaults; the previous
    // provider's redacted keys and clear choices do not carry over.
    redactedKeys = [];
    clearedFields = {};
    fieldValues = defaultsFor(providerName, {});
    initialValues = { ...fieldValues };
  }

  const dispatch = createEventDispatcher<{ close: void }>();

  function body(): Record<string, unknown> {
    return {
      provider: providerName,
      zone,
      record,
      recordType,
      ttl: Number(ttl) || 0,
      enabled,
      // Only edited values (plus explicit clears) are sent, so saving an
      // unrelated field never blanks a stored credential or template.
      fields: changedProviderFields(
        ddnsFields,
        fieldValues,
        initialValues,
        clearedFields,
        redactedKeys
      )
    };
  }

  async function save(runAfter = false) {
    saving = true;
    error = '';
    notice = '';
    try {
      if (!entry || providerName !== loadedProvider) {
        const missing = ddnsFields.filter(
          (field) => field.required && !String(fieldValues[field.key] ?? '').trim()
        );
        if (missing.length > 0) {
          throw new Error(
            `Enter ${missing.map((field) => field.label || field.key).join(', ')} for ${providerName}.`
          );
        }
      }
      const path = entry ? `/api/ddns/${encodeURIComponent(entry.id)}` : '/api/ddns';
      const data = await api<{ id?: string }>(path, {
        method: entry ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body())
      });
      if (runAfter && data.id) {
        running = true;
        try {
          const runData = await api<{ lastIP?: string }>(
            `/api/ddns/${encodeURIComponent(data.id)}/run`,
            { method: 'POST' }
          );
          notice = `Updated: ${runData.lastIP || 'record applied'}`;
        } finally {
          running = false;
        }
        return;
      }
      dispatch('close');
    } catch (e) {
      error = e instanceof ApiError || e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
      running = false;
    }
  }
</script>

<Modal
  open={true}
  title={entry ? 'Edit DDNS Entry' : 'Add DDNS Entry'}
  size="md"
  onClose={() => dispatch('close')}
>
  <form id="ddns-form" class="space-y-4" on:submit|preventDefault={() => save(false)}>
    <FormField label="Provider" id="ddns-provider" hint={currentProvider?.description || ''}>
      <select id="ddns-provider" class="input w-full" value={providerName} on:change={onProviderChange}>
        {#each ddnsProviders as p (p.name)}
          <option value={p.name}>{p.displayName || p.name}</option>
        {/each}
      </select>
    </FormField>

    <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
      <FormField label="Zone" id="ddns-zone">
        <input id="ddns-zone" class="input w-full" bind:value={zone} placeholder="example.com" />
      </FormField>
      <FormField label="Record" id="ddns-record">
        <input id="ddns-record" class="input w-full" bind:value={record} placeholder="home (@ for apex)" />
      </FormField>
    </div>

    <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
      <FormField label="Type" id="ddns-type">
        <select id="ddns-type" class="input w-full" bind:value={recordType}>
          <option value="A">A (IPv4)</option>
          <option value="AAAA">AAAA (IPv6)</option>
        </select>
      </FormField>
      <FormField label="TTL" id="ddns-ttl">
        <input
          id="ddns-ttl"
          class="input w-full"
          type="number"
          min="0"
          bind:value={ttl}
          placeholder="0 = provider default"
        />
      </FormField>
    </div>

    <label class="flex min-h-10 w-fit cursor-pointer select-none items-center gap-2 text-sm">
      <input type="checkbox" bind:checked={enabled} />
      Enabled
    </label>

    <div class="space-y-3 border-t border-naslos-border pt-4">
      <p class="text-xs uppercase tracking-wide text-gray-500">Provider credentials</p>
      <ProviderFields
        fields={ddnsFields}
        values={fieldValues}
        idPrefix="ddns"
        secretSet={!providerChanged ? entry?.credentialFields || [] : []}
        redactedKeys={redactedKeys}
        cleared={clearedFields}
        unchangedPlaceholder="•••••• (unchanged)"
        on:change={() => (fieldValues = { ...fieldValues })}
      />
      {#if redactedKeys.length > 0}
        <p class="text-xs text-gray-500">
          Stored and redacted: <span class="font-mono">{redactedKeys.join(', ')}</span>.
          Their values are not returned; leave a field blank to keep it, or tick Clear to revoke it.
        </p>
      {/if}
    </div>

    {#if notice}
      <div class="callout callout-ok mb-0" role="status">{notice}</div>
    {/if}
    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button
      variant="secondary"
      loading={running}
      disabled={saving || !entry}
      on:click={() => save(true)}
    >{running ? 'Updating...' : 'Update now'}</Button>
    <Button type="submit" form="ddns-form" loading={saving}>
      {saving ? 'Saving...' : 'Save'}
    </Button>
  </svelte:fragment>
</Modal>
