<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import ShareForm from '$lib/components/ShareForm.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { confirmAction } from '$lib/stores/confirm';

  interface Share {
    name: string;
    path: string;
    protocol: string;
    description: string;
    readOnly: boolean;
    browseable: boolean;
    allowedHosts: string[];
    validUsers: string[];
    validGroups: string[];
    timeMachine: boolean;
    createdAt: string;
    enabled: boolean;
  }

  let shares: Share[] = [];
  let loading = true;
  let loadError = '';
  let actionError = '';
  let showForm = false;
  let editingShare: Share | null = null;
  // The host the operator reached the NAS on. SMB is served by a hostNetwork
  // pod on the same node, so this is also the SMB server address. Resolved in
  // onMount because it reads `window` (this component is pre-rendered).
  let host = '';
  let copied = '';
  let busy = '';

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function loadShares() {
    loading = true;
    loadError = '';
    try {
      const data = await api<Share[]>('/api/shares');
      shares = Array.isArray(data) ? data : [];
    } catch (e) {
      shares = [];
      loadError = `Could not load shares: ${message(e)}`;
    } finally {
      loading = false;
    }
  }

  // connectURL returns the address to hand to a client for this share, or an
  // empty string when the protocol has no shareable URL.
  function connectURL(share: Share): string {
    if (!host) return '';
    switch (share.protocol) {
      case 'smb':
        return `smb://${host}/${share.name}`;
      case 'nfs':
        // NFSv4 pseudo path: the address clients use is host:/<share name>,
        // which is also what `mount -t nfs4` takes.
        return `nfs://${host}/${share.name}`;
      default:
        return '';
    }
  }

  async function copyURL(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      copied = url;
      setTimeout(() => {
        if (copied === url) copied = '';
      }, 2000);
    } catch (e) {
      console.error('Failed to copy:', e);
    }
  }

  function newShare() {
    editingShare = null;
    showForm = true;
  }

  function editShare(share: Share) {
    editingShare = share;
    showForm = true;
  }

  async function deleteShare(share: Share) {
    if (
      !(await confirmAction({
        title: 'Delete share',
        message: `Delete share “${share.name}”? The dataset and its files are kept.`,
        confirmLabel: 'Delete',
        tone: 'danger'
      }))
    ) {
      return;
    }
    actionError = '';
    busy = share.name;
    try {
      await api(`/api/shares/${encodeURIComponent(share.name)}`, { method: 'DELETE' });
      await loadShares();
    } catch (e) {
      actionError = `Failed to delete ${share.name}: ${message(e)}`;
    } finally {
      busy = '';
    }
  }

  function protocolIcon(protocol: string): string {
    switch (protocol) {
      case 'smb':
        return 'monitor';
      case 'nfs':
        return 'hard-drive';
      case 'afp':
        return 'folder';
      default:
        return 'folder';
    }
  }

  function protocolLabel(protocol: string): string {
    switch (protocol) {
      case 'smb':
        return 'SMB/CIFS';
      case 'nfs':
        return 'NFS';
      case 'afp':
        return 'AFP (Time Machine)';
      default:
        return protocol;
    }
  }

  onMount(() => {
    host = window.location.hostname;
    loadShares();
  });
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Shares"
    subtitle="Configure SMB, NFS, and Time Machine shares. Connect to an SMB share with the address shown on its card."
    breadcrumbs={[{ label: 'Access' }, { label: 'Shares' }]}
  >
    <Button slot="actions" on:click={newShare}>+ New Share</Button>
  </PageHeader>

  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}

  <AsyncState {loading} error={loadError} loadingText="Loading shares..." onRetry={loadShares}>
    {#if shares.length === 0}
      <EmptyState
        icon="link"
        title="No Shares Configured"
        detail="Create your first share to start serving files to your network."
      >
        <Button slot="action" on:click={newShare}>Create Share</Button>
      </EmptyState>
    {:else}
      <div class="space-y-3">
        {#each shares as share (share.name)}
          <div class="card flex flex-col gap-4 sm:flex-row sm:items-start">
            <div class="text-gray-400" aria-hidden="true">
              <Icon name={protocolIcon(share.protocol)} size={28} />
            </div>
            <div class="min-w-0 flex-1">
              <div class="mb-1 flex flex-wrap items-center gap-2">
                <h2 class="font-bold">{share.name}</h2>
                <span class="badge badge-neutral">{protocolLabel(share.protocol)}</span>
                {#if share.timeMachine}
                  <span class="badge badge-info">Time Machine</span>
                {/if}
                {#if !share.enabled}
                  <StatusBadge state="disabled" label="Disabled" tone="warn" />
                {/if}
              </div>
              <p class="break-all font-mono text-sm text-gray-400">{share.path}</p>
              {#if share.description}
                <p class="mt-1 text-sm text-gray-500">{share.description}</p>
              {/if}
              {#if (share.validGroups?.length ?? 0) > 0 || (share.validUsers?.length ?? 0) > 0}
                <p class="mt-1 text-xs text-gray-500">
                  Access:
                  {#if (share.validUsers?.length ?? 0) > 0}
                    <span class="text-gray-400">users {share.validUsers.join(', ')}</span>
                  {/if}
                  {#if (share.validUsers?.length ?? 0) > 0 && (share.validGroups?.length ?? 0) > 0}<span class="text-gray-500"> · </span>{/if}
                  {#if (share.validGroups?.length ?? 0) > 0}
                    <span class="text-gray-400">groups {share.validGroups.join(', ')}</span>
                  {/if}
                </p>
              {/if}
              {#if connectURL(share)}
                <div class="mt-2 flex flex-wrap items-center gap-2">
                  <code class="select-all rounded border border-naslos-border bg-naslos-dark px-2 py-1 text-xs text-naslos-link">{connectURL(share)}</code>
                  <Button
                    size="sm"
                    variant="secondary"
                    title="Copy the share address"
                    on:click={() => copyURL(connectURL(share))}
                  >{copied === connectURL(share) ? 'Copied' : 'Copy'}</Button>
                </div>
              {:else if !share.enabled}
                <p class="mt-2 text-xs text-gray-500">Disabled — not reachable until enabled.</p>
              {/if}
            </div>
            <div class="flex flex-wrap gap-2">
              <Button size="sm" variant="secondary" on:click={() => editShare(share)}>Edit</Button>
              <Button
                size="sm"
                variant="danger"
                loading={busy === share.name}
                disabled={!!busy && busy !== share.name}
                on:click={() => deleteShare(share)}
              >Delete</Button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </AsyncState>
</div>

{#if showForm}
  <ShareForm
    share={editingShare}
    on:close={() => {
      showForm = false;
      loadShares();
    }}
  />
{/if}
