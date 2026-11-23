<script lang="ts">
  import { modal } from '$lib/actions/modal';
  import Icon from '$lib/components/Icon.svelte';
  import { createEventDispatcher, onMount } from 'svelte';
  import ProviderFields from './ProviderFields.svelte';
  import InfoBubble from './InfoBubble.svelte';
  import { changedProviderFields, providerDefaults, visibleFields } from '$lib/providerFields';
  import type { DdnsEntryView, DnsProviderView, DomainView } from '$lib/dnsTypes';

  export let domain: DomainView | null = null;

  // Which provider the loaded credentials belong to. A provider change must
  // never treat the old Secret as covering the new provider's fields.
  const loadedProvider = domain?.dnsProvider ?? '';

  let baseDomain = domain?.baseDomain || '';
  let dnsProvider = domain?.dnsProvider || 'cloudflare';
  let credentialsSecret = domain?.credentialsSecret || '';
  let acmeEmail = domain?.acmeEmail || '';
  let environment = domain?.environment || 'staging';
  // The solver body is never returned; only its presence is reported.
  let solverText = '';
  let solverConfigured = !!domain?.solverConfigured;
  let saving = false;
  let error = '';
  let providers: DnsProviderView[] = [];
  // Credential-bearing and free-form providerConfig keys are redacted from the
  // response and must not be prefilled or blanked by an unrelated save.
  let redactedKeys: string[] = Array.isArray(domain?.redactedProviderConfigKeys)
    ? [...domain.redactedProviderConfigKeys]
    : [];
  let fieldValues: Record<string, string> = { ...(domain?.providerConfig || {}) };
  let initialValues: Record<string, string> = { ...fieldValues };
  let clearedFields: Record<string, boolean> = {};
  let ddnsEntries: DdnsEntryView[] = [];
  let ddnsEnabled = false;
  // pending = the GET has not answered yet; failed must not look like "no
  // entries exist" or a save would detach every app CNAME.
  let ddnsState: 'pending' | 'ok' | 'failed' = 'pending';
  let appRecordsEntry = domain?.appRecordsDdnsEntry || '';
  let appRecordsEnabled = !!appRecordsEntry;

  const dispatch = createEventDispatcher();

  async function loadDdns() {
    ddnsState = 'pending';
    try {
      const res = await fetch('/api/ddns');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const ddns = await res.json();
      ddnsEnabled = ddns.enabled !== false;
      ddnsEntries = (Array.isArray(ddns.entries) ? ddns.entries : []).filter(
        (entry: DdnsEntryView) => typeof entry.record === 'string' && !entry.record.startsWith('*.')
      );
      ddnsState = 'ok';
    } catch {
      // Keep the stored app-record target values and disable the control; the
      // save path below preserves them instead of sending null.
      ddnsState = 'failed';
    }
  }

  async function loadProviders() {
    try {
      const res = await fetch('/api/providers');
      if (!res.ok) return;
      const data = await res.json();
      providers = (Array.isArray(data.providers) ? data.providers : []).filter(
        (p: DnsProviderView) => p.certManager
      );
      if (!providers.some((p) => p.name === dnsProvider) && providers.length) {
        dnsProvider = providers[0].name;
      }
      fieldValues = defaultsFor(dnsProvider, fieldValues);
      initialValues = { ...fieldValues };
      clearedFields = {};
    } catch {
      // The hardcoded fallbacks below keep the form usable if the API is down.
    }
  }

  onMount(() => {
    loadProviders();
    loadDdns();
  });

  // Svelte 5 does not track state read inside a function called from the
  // template, so derive the provider/fields reactively instead.
  $: currentProvider = providers.find((p) => p.name === dnsProvider);
  $: currentFields = currentProvider?.fields ?? [];
  // Hide DDNS-only fields; showIf rules whose controller is absent here (no
  // `mode` in the cert form) are ignored, so shared fields stay visible.
  $: certFields = visibleFields(
    currentFields.filter((f) => f.scope !== 'ddns'),
    fieldValues
  );
  $: appRecordsSupported = ['ovh', 'cloudflare'].includes(dnsProvider);
  $: appRecordsUnavailable =
    ddnsState !== 'ok' || !appRecordsSupported || !ddnsEnabled || ddnsEntries.length === 0;
  // Only a successful, complete DDNS load may clear the stored selection.
  $: if (ddnsState === 'ok' && appRecordsSupported && (appRecordsUnavailable || !ddnsEnabled)) {
    appRecordsEnabled = false;
  }
  $: if (ddnsState === 'ok' && appRecordsEnabled && !appRecordsEntry && ddnsEntries.length > 0) {
    appRecordsEntry = ddnsEntries[0].id;
  }
  $: if (
    ddnsState === 'ok' &&
    appRecordsEnabled &&
    appRecordsEntry &&
    !ddnsEntries.some((entry) => entry.id === appRecordsEntry)
  ) {
    appRecordsEntry = ddnsEntries.length > 0 ? ddnsEntries[0].id : '';
  }
  // An edit whose provider changed no longer has "already stored" credentials
  // for the selected provider: every required value must be entered again.
  $: providerChanged = !!domain && dnsProvider !== loadedProvider;

  function entryLabel(entry: DdnsEntryView): string {
    return entry.record === '@' || !entry.record ? entry.zone : `${entry.record}.${entry.zone}`;
  }

  function fieldsFor(name: string) {
    const selected = providers.find((p) => p.name === name);
    return (selected?.fields ?? []).filter((f) => f.scope !== 'ddns');
  }

  function defaultsFor(name: string, seed: Record<string, string>): Record<string, string> {
    return providerDefaults(fieldsFor(name), seed, redactedKeys);
  }

  function onProviderChange(event: Event) {
    const nextName = (event.currentTarget as HTMLSelectElement).value;
    if (nextName === dnsProvider) return;
    dnsProvider = nextName;
    // The old provider's Secret, redacted keys and clear choices do not belong
    // to the new provider. Clearing the name lets the server derive/accept a
    // fresh Secret when the new provider needs one.
    credentialsSecret = '';
    redactedKeys = [];
    clearedFields = {};
    fieldValues = defaultsFor(dnsProvider, {});
    initialValues = { ...fieldValues };
    solverConfigured = false;
    solverText = '';
  }

  async function save() {
    saving = true;
    error = '';
    try {
      if (ddnsState !== 'ok' && !domain) {
        throw new Error('Wait for the DDNS entries to load before saving.');
      }
      const appRecordTarget =
        ddnsState === 'ok'
          ? appRecordsEnabled && appRecordsEntry
            ? appRecordsEntry
            : null
          : (domain?.appRecordsDdnsEntry ?? null);
      const body: Record<string, unknown> = {
        baseDomain,
        dnsProvider,
        credentialsSecret,
        acmeEmail,
        environment,
        appRecordsDdnsEntry: appRecordTarget
      };
      if (dnsProvider === 'passthrough') {
        const text = solverText.trim();
        if (text !== '') {
          try {
            body.solver = JSON.parse(text);
          } catch {
            throw new Error('Solver must be valid JSON');
          }
        } else if (!solverConfigured) {
          throw new Error('A solver is required for the passthrough provider.');
        }
      } else if (certFields.length > 0) {
        if (!domain || dnsProvider !== loadedProvider) {
          const missing = certFields.filter(
            (field) => field.required && !String(fieldValues[field.key] ?? '').trim()
          );
          if (missing.length > 0) {
            throw new Error(
              `Enter ${missing.map((field) => field.label || field.key).join(', ')} for ${dnsProvider}.`
            );
          }
        }
        // Send only the fields the operator edited. Omitted (and blank)
        // write-only fields keep their stored value instead of clearing it.
        // Send `fields` even when nothing changed: the server's resolver then
        // preserves the stored safe config and credentials. Omitting the key
        // entirely would replace providerConfig with an empty map.
        body.fields = changedProviderFields(
          certFields,
          fieldValues,
          initialValues,
          clearedFields,
          redactedKeys
        );
      }
      const res = await fetch(`/api/domains/${encodeURIComponent(baseDomain)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Save failed');
      }
      dispatch('close');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" use:modal={{ onClose: () => dispatch('close') }} on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{domain ? 'Edit Domain' : 'Add Domain'}</h2>
      <button class="icon-btn -mr-2 text-gray-400 hover:text-naslos-strong hover:bg-naslos-border" aria-label="Close" on:click={() => dispatch('close')}><Icon name="x" /></button>
    </div>
    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label" for="domain-name">Base domain</label>
        <input id="domain-name" class="input" bind:value={baseDomain} placeholder="example.com" disabled={!!domain} />
      </div>
      <div>
        <div class="flex items-center gap-2 mb-1">
          <label class="label mb-0" for="domain-provider">DNS-01 provider</label>
          <InfoBubble items={currentProvider?.apiRights ?? []} />
        </div>
        <select id="domain-provider" class="input" value={dnsProvider} on:change={onProviderChange}>
          {#if providers.length === 0}
            <option value="cloudflare">Cloudflare</option>
            <option value="rfc2136">RFC2136</option>
            <option value="passthrough">Passthrough (raw solver)</option>
          {:else}
            {#each providers as p}
              <option value={p.name}>{p.displayName || p.name}</option>
            {/each}
          {/if}
        </select>
        {#if currentProvider?.description}<p class="text-xs text-gray-500 mt-1">{currentProvider?.description}</p>{/if}
      </div>

      {#if dnsProvider === 'passthrough'}
        <div>
          <label class="label" for="domain-solver">Solver (JSON)</label>
          <textarea
            id="domain-solver"
            class="input h-32 font-mono text-sm"
            bind:value={solverText}
            placeholder={solverConfigured ? 'Configured — paste a replacement solver to change it' : ''}
          ></textarea>
          {#if solverConfigured}
            <p class="text-xs text-amber-300 mt-1">
              A solver is configured; its body is not returned. Leave this blank to keep the stored
              solver, or paste a replacement.
            </p>
          {/if}
        </div>
      {:else if certFields.length > 0}
        <ProviderFields
          fields={certFields}
          values={fieldValues}
          idPrefix="domain"
          secretSet={!providerChanged && domain?.credentialsSecret
            ? certFields.filter((f) => f.secret).map((f) => f.key)
            : []}
          redactedKeys={redactedKeys}
          cleared={clearedFields}
          unchangedPlaceholder="•••••• (unchanged)"
          on:change={() => (fieldValues = { ...fieldValues })}
        />
        {#if redactedKeys.length > 0}
          <p class="text-xs text-gray-500">
            Stored and redacted: <span class="font-mono">{redactedKeys.join(', ')}</span>.
            Their values are not returned; leave a field blank to keep it, or tick Clear to remove it.
          </p>
        {/if}
      {:else}
        <div>
          <label class="label" for="domain-secret">Credentials Secret</label>
          <input id="domain-secret" class="input" bind:value={credentialsSecret} placeholder="cloudflare-api-token" />
        </div>
      {/if}

      <div>
        <label class="label" for="domain-email">ACME email</label>
        <input id="domain-email" class="input" bind:value={acmeEmail} />
      </div>
      <div>
        <label class="label" for="domain-env">Environment</label>
        <select id="domain-env" class="input" bind:value={environment}>
          <option value="staging">Staging</option>
          <option value="production">Production</option>
        </select>
      </div>

      <div class="border-t border-naslos-border pt-4">
        <label class="flex items-start gap-2 cursor-pointer" class:opacity-60={appRecordsUnavailable}>
          <input
            type="checkbox"
            class="mt-1"
            bind:checked={appRecordsEnabled}
            disabled={appRecordsUnavailable || ddnsState !== 'ok'}
          />
          <span class="text-sm">
            Create DNS records for apps
            <span class="block text-xs text-gray-500">
              Every exposed app on this domain gets a CNAME pointing at the selected DDNS
              hostname (TLS is unaffected: the wildcard certificate already covers app hosts).
            </span>
          </span>
        </label>
        {#if ddnsState === 'failed'}
          <p class="text-xs text-amber-300 mt-2" role="alert">
            Could not load the DDNS entries. The stored app-record target
            {#if domain?.appRecordsDdnsEntry}
              is kept unchanged
            {:else}
              cannot be selected
            {/if}
            and the control is disabled.
            <button type="button" class="underline hover:text-amber-200" on:click={loadDdns}>Retry</button>
          </p>
        {:else if ddnsState === 'pending'}
          <p class="text-xs text-gray-500 mt-2">Loading DDNS entries…</p>
        {:else if appRecordsUnavailable}
          <p class="text-xs text-amber-300 mt-2">
            {#if !appRecordsSupported}
              Only OVH (ZoneDNS API keys) and Cloudflare domains can create app records.
            {:else if !ddnsEnabled}
              Dynamic DNS is disabled; enable it and add an entry first.
            {:else}
              No non-wildcard DDNS entry exists yet; add one under DNS first.
            {/if}
          </p>
        {:else if appRecordsEnabled}
          <div class="mt-3">
            <label class="label" for="domain-app-target">App records target</label>
            <select id="domain-app-target" class="input" bind:value={appRecordsEntry}>
              {#each ddnsEntries as entry (entry.id)}
                <option value={entry.id}>{entryLabel(entry)} ({entry.provider})</option>
              {/each}
            </select>
          </div>
        {/if}
      </div>
      {#if error}<div role="alert" class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
