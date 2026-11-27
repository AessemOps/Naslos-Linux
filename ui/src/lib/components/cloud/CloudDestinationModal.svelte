<script lang="ts">
  // Add/edit an S3 destination. Credentials are write-only: the API reports
  // only hasAccessKey/hasSecretKey/hasSessionToken, the inputs are never
  // prefilled, and a blank input on edit keeps the stored value (the field is
  // left out of the request body entirely).
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { isHttpEndpoint, type Destination, type DestinationInput } from '$lib/cloud/types';

  export let destination: Destination | null = null;

  const dispatch = createEventDispatcher<{ saved: Destination; close: void }>();
  const KEEP = 'Configured — leave blank to keep';

  $: editing = !!destination;
  let form = {
    name: destination?.name ?? '',
    endpoint: destination?.endpoint ?? 'https://',
    region: destination?.region ?? 'us-east-1',
    bucket: destination?.bucket ?? '',
    prefix: destination?.prefix ?? '',
    pathStyle: destination?.pathStyle ?? false,
    enabled: destination?.enabled ?? true,
    insecureAck: destination?.allowInsecureHttp ?? false
  };
  // Never bound to a stored value: these hold only what the user types now.
  let secrets = { accessKeyId: '', secretAccessKey: '', sessionToken: '' };

  let submitted = false;
  let saving = false;
  let saveError = '';

  $: http = isHttpEndpoint(form.endpoint);
  $: errors = validate(form, secrets);
  $: shown = submitted ? errors : {};
  $: blocked = http && !form.insecureAck;

  function validate(f: typeof form, s: typeof secrets): Record<string, string> {
    const e: Record<string, string> = {};
    if (!f.name.trim()) e.name = 'Name is required.';
    const ep = f.endpoint.trim();
    if (!/^https?:\/\/[^/\s]+/i.test(ep)) e.endpoint = 'Enter an http:// or https:// endpoint URL.';
    if (!f.bucket.trim()) e.bucket = 'Bucket is required.';
    if (!editing || !destination?.hasAccessKey) {
      if (!s.accessKeyId.trim()) e.accessKeyId = 'Access key ID is required.';
    }
    if (!editing || !destination?.hasSecretKey) {
      if (!s.secretAccessKey) e.secretAccessKey = 'Secret access key is required.';
    }
    return e;
  }

  function body(): DestinationInput {
    const out: DestinationInput = {
      name: form.name.trim(),
      endpoint: form.endpoint.trim(),
      region: form.region.trim(),
      bucket: form.bucket.trim(),
      prefix: form.prefix.trim(),
      pathStyle: form.pathStyle,
      allowInsecureHttp: http && form.insecureAck,
      enabled: form.enabled
    };
    // Blank means "keep" (edit) or "not set" (create): never send it.
    if (secrets.accessKeyId.trim()) out.accessKeyId = secrets.accessKeyId.trim();
    if (secrets.secretAccessKey) out.secretAccessKey = secrets.secretAccessKey;
    if (secrets.sessionToken) out.sessionToken = secrets.sessionToken;
    return out;
  }

  async function save() {
    submitted = true;
    saveError = '';
    if (Object.keys(errors).length > 0 || blocked) return;
    saving = true;
    try {
      const path = destination
        ? `/api/cloud/destinations/${encodeURIComponent(destination.id)}`
        : '/api/cloud/destinations';
      const res = await api<{ destination: Destination }>(path, {
        method: destination ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body())
      });
      secrets = { accessKeyId: '', secretAccessKey: '', sessionToken: '' };
      dispatch('saved', res.destination);
    } catch (e) {
      saveError = 'Saving the destination failed: ' + (e instanceof ApiError || e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  function describedBy(id: string, hint = false): string | undefined {
    const ids = [hint ? `${id}-hint` : '', shown[id.replace('dest-', '')] ? `${id}-error` : ''].filter(Boolean);
    return ids.length ? ids.join(' ') : undefined;
  }
</script>

<Modal title={editing ? `Edit destination ${destination?.name}` : 'Add cloud destination'} size="lg" onClose={() => dispatch('close')}>
  <form id="cloud-destination-form" class="grid grid-cols-1 gap-3 sm:grid-cols-2" novalidate on:submit|preventDefault={save}>
    <FormField label="Name" id="dest-name" required error={shown.name}>
      <input id="dest-name" class="input w-full" bind:value={form.name} aria-describedby={describedBy('dest-name')} />
    </FormField>
    <FormField label="Endpoint" id="dest-endpoint" required error={shown.endpoint} hint="For example https://s3.eu-central-1.amazonaws.com">
      <input
        id="dest-endpoint"
        class="input w-full"
        type="url"
        bind:value={form.endpoint}
        aria-describedby={describedBy('dest-endpoint', true)}
      />
    </FormField>
    <FormField label="Region" id="dest-region">
      <input id="dest-region" class="input w-full" bind:value={form.region} />
    </FormField>
    <FormField label="Bucket" id="dest-bucket" required error={shown.bucket}>
      <input id="dest-bucket" class="input w-full" bind:value={form.bucket} aria-describedby={describedBy('dest-bucket')} />
    </FormField>
    <FormField label="Prefix" id="dest-prefix" hint="Optional key prefix inside the bucket.">
      <input id="dest-prefix" class="input w-full" bind:value={form.prefix} aria-describedby="dest-prefix-hint" />
    </FormField>
    <div class="flex flex-col justify-end gap-2 text-sm">
      <label class="flex items-center gap-2">
        <input id="dest-path-style" type="checkbox" class="h-4 w-4 rounded" bind:checked={form.pathStyle} />
        Path-style addressing (MinIO and most self-hosted S3)
      </label>
      <label class="flex items-center gap-2">
        <input id="dest-enabled" type="checkbox" class="h-4 w-4 rounded" bind:checked={form.enabled} />
        Enabled
      </label>
    </div>

    <FormField label="Access key ID" id="dest-accessKeyId" required={!destination?.hasAccessKey} error={shown.accessKeyId}>
      <input
        id="dest-accessKeyId"
        class="input w-full"
        type="password"
        autocomplete="off"
        placeholder={destination?.hasAccessKey ? KEEP : ''}
        bind:value={secrets.accessKeyId}
        aria-describedby={describedBy('dest-accessKeyId')}
      />
    </FormField>
    <FormField label="Secret access key" id="dest-secretAccessKey" required={!destination?.hasSecretKey} error={shown.secretAccessKey}>
      <input
        id="dest-secretAccessKey"
        class="input w-full"
        type="password"
        autocomplete="off"
        placeholder={destination?.hasSecretKey ? KEEP : ''}
        bind:value={secrets.secretAccessKey}
        aria-describedby={describedBy('dest-secretAccessKey')}
      />
    </FormField>
    <FormField label="Session token" id="dest-sessionToken" hint="Optional; only for temporary credentials.">
      <input
        id="dest-sessionToken"
        class="input w-full"
        type="password"
        autocomplete="off"
        placeholder={destination?.hasSessionToken ? KEEP : ''}
        bind:value={secrets.sessionToken}
        aria-describedby="dest-sessionToken-hint"
      />
    </FormField>

    {#if http}
      <div class="rounded border border-amber-600 bg-amber-900/10 p-3 text-sm sm:col-span-2">
        <label class="flex items-start gap-2">
          <input id="dest-insecure-ack" type="checkbox" class="mt-0.5 h-4 w-4 rounded" bind:checked={form.insecureAck} />
          <span>
            I accept plaintext <span class="font-mono">http</span>: the S3 credentials and request
            signatures travel unencrypted on the network. Chunk contents stay encrypted. The
            acknowledgement is audited.
          </span>
        </label>
        {#if blocked}
          <p class="mt-2 text-xs text-amber-300">Confirm the plaintext-http risk to save this destination.</p>
        {/if}
      </div>
    {/if}
  </form>

  {#if saveError}
    <p role="alert" class="text-sm text-red-300">{saveError}</p>
  {/if}

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button type="submit" form="cloud-destination-form" loading={saving} disabled={blocked}>
      {editing ? 'Save changes' : 'Add destination'}
    </Button>
  </svelte:fragment>
</Modal>
