<script lang="ts">
  // Mandatory recovery-key acknowledgement before the first cloud send or
  // schedule. The text states what a restore needs; it does not claim that
  // Naslos verified any export.
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { CLOUD_DOCS_URL } from '$lib/cloud/drAck';

  export let acknowledged = false;

  const dispatch = createEventDispatcher<{ acknowledge: void }>();

  let checked = false;
</script>

{#if acknowledged}
  <details class="card mb-6 text-sm" data-testid="cloud-dr-acknowledged">
    <summary class="cursor-pointer font-medium">Recovery keys acknowledged in this browser</summary>
    <div class="mt-3 space-y-2 text-gray-300">
      <p>
        A restore on another NAS needs the Buddy identity, the key-encryption key (KEK), and the
        ZFS key of every encrypted dataset. See
        <a class="text-naslos-link underline" href={CLOUD_DOCS_URL} target="_blank" rel="noopener noreferrer"
          >docs/cloud-backup.md</a
        >.
      </p>
    </div>
  </details>
{:else}
  <section
    class="card mb-6 border-amber-600 bg-amber-900/10"
    aria-labelledby="cloud-dr-heading"
    data-testid="cloud-dr-panel"
  >
    <h2 id="cloud-dr-heading" class="mb-2 flex items-center gap-2 text-lg font-bold">
      <Icon name="triangle-alert" size={20} />
      Before the first cloud backup
    </h2>
    <div class="space-y-2 text-sm text-gray-300">
      <p>
        The bucket holds only encrypted chunks and signed manifests. The keys that decrypt and
        verify them are stored on this NAS. If this NAS is lost, the bucket alone cannot be
        restored.
      </p>
      <p>Back up the following off the NAS before you rely on cloud backups:</p>
      <ul class="list-disc space-y-1 pl-5">
        <li>The Buddy backup identity. Its signing key verifies every manifest.</li>
        <li>The key-encryption key (KEK) that seals the chunk keys.</li>
        <li>
          For encrypted datasets, the ZFS dataset key. Sends are raw, so the data stays encrypted
          with that key, and Naslos does not store it in the bucket.
        </li>
      </ul>
      <p>
        The export steps are in
        <a class="text-naslos-link underline" href={CLOUD_DOCS_URL} target="_blank" rel="noopener noreferrer"
          >docs/cloud-backup.md</a
        >. Naslos does not check that the export was done. This acknowledgement is stored only in
        this browser.
      </p>
    </div>
    <label class="mt-4 flex items-start gap-2 text-sm">
      <input id="cloud-dr-check" type="checkbox" class="mt-0.5 h-4 w-4 rounded" bind:checked />
      <span>I have backed up the identity, the KEK and the keys of encrypted datasets off this NAS.</span>
    </label>
    <div class="mt-3">
      <Button disabled={!checked} on:click={() => dispatch('acknowledge')}>Confirm recovery keys</Button>
    </div>
  </section>
{/if}
