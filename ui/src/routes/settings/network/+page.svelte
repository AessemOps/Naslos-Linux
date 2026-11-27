<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import ApplianceCard from '$lib/components/network/ApplianceCard.svelte';
  import HostnameCard from '$lib/components/network/HostnameCard.svelte';
  import LinkCard from '$lib/components/network/LinkCard.svelte';
  import PendingChangeCard from '$lib/components/network/PendingChangeCard.svelte';
  import ResolversCard from '$lib/components/network/ResolversCard.svelte';
  import type { Inventory } from '$lib/components/network/network';
  import { emptyInventory } from '$lib/components/network/network';
  import { api } from '$lib/api';

  let inventory: Inventory = emptyInventory;
  let loading = true;
  let loadError = '';
  let saving = false;
  let message = '';
  let messageType: 'success' | 'error' | 'info' = 'success';
  let nowMs = Date.now();

  // Confirmation modal.
  let showConfirm = false;
  let confirmKind: 'link' | 'dns' = 'link';
  let confirmSummary = '';
  let confirmBody: Record<string, unknown> = {};
  let confirmResolver: ((ok: boolean) => void) | null = null;

  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let clockTimer: ReturnType<typeof setInterval> | null = null;
  let destroyed = false;

  async function load() {
    loadError = '';
    try {
      const next = await api<Inventory>('/api/network');
      if (destroyed) return;
      inventory = next;
      ensurePolling();
    } catch (e) {
      if (!destroyed) {
        loadError = 'Failed to load the network inventory: ' + (e instanceof Error ? e.message : String(e));
      }
    } finally {
      loading = false;
    }
  }

  function retryLoad() {
    loading = true;
    load();
  }

  function ensurePolling() {
    if (!inventory.pending) {
      stopPolling();
      return;
    }
    if (pollTimer) return;
    pollTimer = setInterval(async () => {
      const id = inventory.pending ? inventory.pending.id : '';
      if (!id) {
        stopPolling();
        return;
      }
      try {
        const change = await api<Inventory['pending']>(`/api/network/changes/${id}`);
        if (destroyed || !change) return;
        const done = ['applied', 'reverted', 'failed'].includes(change.phase);
        inventory = { ...inventory, pending: done ? null : change };
        if (done) {
          stopPolling();
          await load();
        }
      } catch {
        // Keep polling: the API pod may be restarting with the node.
      }
    }, 2000);
  }

  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  // The link editor awaits this promise and closes only on an accepted change,
  // so cancelling the dialog (or a failed POST) keeps its draft.
  function requestChange(
    body: Record<string, unknown>,
    summary: string,
    kind: 'link' | 'dns'
  ): Promise<boolean> {
    if (inventory.pending) return Promise.resolve(false);
    if (confirmResolver) settleConfirm(false);
    confirmKind = kind;
    confirmBody = body;
    confirmSummary = summary;
    showConfirm = true;
    return new Promise<boolean>((resolve) => {
      confirmResolver = resolve;
    });
  }

  function settleConfirm(ok: boolean) {
    showConfirm = false;
    const resolve = confirmResolver;
    confirmResolver = null;
    resolve?.(ok);
  }

  async function confirmApply() {
    saving = true;
    message = '';
    try {
      await api('/api/network/changes', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(confirmBody)
      });
      message = 'Change started. The node is being reconfigured behind a 60 second guard.';
      messageType = 'success';
      settleConfirm(true);
      await load();
    } catch (e) {
      message = 'Could not start the change: ' + (e instanceof Error ? e.message : String(e));
      messageType = 'error';
      settleConfirm(false);
    } finally {
      saving = false;
    }
  }

  async function revertNow() {
    const pending = inventory.pending;
    if (!pending) return;
    saving = true;
    try {
      await api(`/api/network/changes/${pending.id}/revert`, { method: 'POST' });
      message = 'Reverting to the saved configuration.';
      messageType = 'success';
      await load();
    } catch (e) {
      message = 'Could not revert: ' + (e instanceof Error ? e.message : String(e));
      messageType = 'error';
    } finally {
      saving = false;
    }
  }

  $: pendingSeconds = inventory.pending
    ? Math.max(0, Math.round((new Date(inventory.pending.deadline).getTime() - nowMs) / 1000))
    : 0;

  onMount(() => {
    load();
    clockTimer = setInterval(() => {
      nowMs = Date.now();
    }, 1000);
  });

  onDestroy(() => {
    destroyed = true;
    stopPolling();
    if (clockTimer) clearInterval(clockTimer);
  });
</script>

<div class="mx-auto max-w-4xl">
  <PageHeader
    title="Network"
    subtitle="Host links, addressing, MTU and DNS, applied through a guarded Talos machine configuration."
  >
    <div slot="actions">
      <Button variant="secondary" loading={loading} on:click={load}>Reload</Button>
    </div>
  </PageHeader>

  {#if message}
    <div
      class={`callout ${messageType === 'success' ? 'callout-ok' : 'callout-bad'}`}
      role={messageType === 'success' ? 'status' : 'alert'}
    >
      <span>{message}</span>
    </div>
  {/if}

  {#if loading}
    <AsyncState loading loadingText="Loading network inventory..." />
  {:else}
    {#if loadError}
      <div class="callout callout-bad mb-4" role="alert">
        <div class="flex flex-wrap items-start justify-between gap-3">
          <p class="text-sm">{loadError}</p>
          <Button variant="secondary" size="sm" on:click={retryLoad}>Retry</Button>
        </div>
        <p class="mt-2 text-xs opacity-80">
          The inventory could not be read. The sections below show the last known values and
          changes stay disabled until a reload succeeds.
        </p>
      </div>
    {/if}

    {#if inventory.pending}
      <PendingChangeCard
        pending={inventory.pending}
        {pendingSeconds}
        {saving}
        onRevert={revertNow}
      />
    {/if}

    <HostnameCard
      value={inventory.hostname.value}
      source={inventory.hostname.source}
    />

    <ResolversCard
      resolvers={inventory.resolvers}
      primaryLink={inventory.primaryLink}
      disabled={saving || inventory.pending !== null || loadError !== ''}
      {saving}
      on:apply={(event) => void requestChange(event.detail.body, event.detail.summary, 'dns')}
    />

    <section class="mb-6" aria-labelledby="network-links">
      <h2 id="network-links" class="mb-3 text-lg font-bold">Links</h2>
      <div class="space-y-4">
        {#each inventory.links as link (link.name)}
          <LinkCard
            {link}
            disabled={saving || inventory.pending !== null || loadError !== ''}
            {saving}
            onApply={(body, summary) => requestChange(body, summary, 'link')}
          />
        {/each}
        {#if inventory.links.length === 0}
          <p class="text-gray-400">No physical links were reported by Talos.</p>
        {/if}
      </div>
    </section>

    <ApplianceCard
      domain={inventory.appliance.domain}
      lanCidr={inventory.appliance.lanCidr}
      primaryLink={inventory.primaryLink}
    />
  {/if}
</div>

{#if showConfirm}
  <Modal
    title="Confirm the network change"
    onClose={() => {
      if (!saving) settleConfirm(false);
    }}
  >
    <p class="text-sm text-gray-300">{confirmSummary}</p>
    {#if confirmKind === 'link'}
      <div class="callout callout-warn space-y-2 text-sm">
        <p>
          If this change moves the node's address, this browser will lose the page. Re-point DNS
          or your <span class="font-mono">/etc/hosts</span> entry at the new address afterwards.
        </p>
        <p>
          The node starts a 60 second guarded apply and reverts by itself if the new address never
          answers. The API adds a five minute watchdog and a saved full-config revert.
        </p>
      </div>
    {/if}
    <div slot="footer">
      <Button variant="primary" loading={saving} on:click={confirmApply}>Apply</Button>
      <Button variant="secondary" disabled={saving} on:click={() => settleConfirm(false)}>
        Cancel
      </Button>
    </div>
  </Modal>
{/if}
