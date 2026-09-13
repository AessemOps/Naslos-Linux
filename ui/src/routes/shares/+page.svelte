<script lang="ts">
  import { onMount } from 'svelte';
  import ShareForm from '$lib/components/ShareForm.svelte';

  interface Share {
    name: string;
    path: string;
    protocol: string;
    description: string;
    readOnly: boolean;
    browseable: boolean;
    allowedHosts: string[];
    validUsers: string[];
    timeMachine: boolean;
    createdAt: string;
    enabled: boolean;
  }

  let shares: Share[] = [];
  let loading = true;
  let showForm = false;
  let editingShare: Share | null = null;
  // The host the operator reached the NAS on. SMB is served by a hostNetwork
  // pod on the same node, so this is also the SMB server address. Resolved in
  // onMount because it reads `window` (this component is pre-rendered).
  let host = '';
  let copied = '';

  async function loadShares() {
    loading = true;
    try {
      const res = await fetch('/api/shares');
      const data = await res.json();
      shares = Array.isArray(data) ? data : [];
    } catch (e) {
      console.error('Failed to load shares:', e);
      shares = [];
    } finally {
      loading = false;
    }
  }

  // connectURL returns the address to hand to a client for this share, or an
  // empty string when the protocol has no shareable URL (or nothing serves it
  // yet — NFS is not implemented on Talos, which has no kernel nfsd).
  function connectURL(share: Share): string {
    if (!host) return '';
    switch (share.protocol) {
      case 'smb':
        return `smb://${host}/${share.name}`;
      default:
        return '';
    }
  }

  async function copyURL(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      copied = url;
      setTimeout(() => { if (copied === url) copied = ''; }, 2000);
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

  async function deleteShare(name: string) {
    if (!confirm(`Delete share "${name}"?`)) return;
    try {
      await fetch(`/api/shares/${name}`, { method: 'DELETE' });
      loadShares();
    } catch (e) {
      console.error('Failed to delete share:', e);
    }
  }

  function protocolIcon(protocol: string): string {
    switch (protocol) {
      case 'smb': return '🪟';
      case 'nfs': return '🐧';
      case 'afp': return '🍎';
      default: return '📁';
    }
  }

  function protocolLabel(protocol: string): string {
    switch (protocol) {
      case 'smb': return 'SMB/CIFS';
      case 'nfs': return 'NFS';
      case 'afp': return 'AFP (Time Machine)';
      default: return protocol;
    }
  }

  onMount(() => {
    host = window.location.hostname;
    loadShares();
  });
</script>

<div class="max-w-5xl mx-auto">
  <div class="flex justify-between items-center mb-6">
    <div>
      <h1 class="text-3xl font-bold mb-2">Shares</h1>
      <p class="text-gray-400">Configure SMB, NFS, and Time Machine shares. Connect to an SMB share with the address shown on its card.</p>
    </div>
    <button class="btn btn-primary" on:click={newShare}>+ New Share</button>
  </div>

  {#if loading}
    <p class="text-gray-400">Loading shares...</p>
  {:else if shares.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">🔗</div>
      <h2 class="text-xl font-bold mb-2">No Shares Configured</h2>
      <p class="text-gray-400 mb-4">Create your first share to start serving files to your network.</p>
      <button class="btn btn-primary" on:click={newShare}>Create Share</button>
    </div>
  {:else}
    <div class="space-y-3">
      {#each shares as share}
        <div class="card flex items-center gap-4">
          <div class="text-3xl">{protocolIcon(share.protocol)}</div>
          <div class="flex-1">
            <div class="flex items-center gap-2 mb-1">
              <h3 class="font-bold">{share.name}</h3>
              <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{protocolLabel(share.protocol)}</span>
              {#if share.timeMachine}<span class="text-xs px-2 py-0.5 rounded bg-blue-900/50 text-blue-300">Time Machine</span>{/if}
              {#if !share.enabled}<span class="text-xs px-2 py-0.5 rounded bg-yellow-900/50 text-yellow-300">Disabled</span>{/if}
            </div>
            <p class="text-sm text-gray-400">{share.path}</p>
            {#if share.description}<p class="text-sm text-gray-500 mt-1">{share.description}</p>{/if}
            {#if connectURL(share)}
              <div class="flex items-center gap-2 mt-2">
                <code class="text-xs bg-naslos-dark border border-naslos-border rounded px-2 py-1 text-naslos-primary select-all">{connectURL(share)}</code>
                <button
                  class="text-xs px-2 py-1 rounded border border-naslos-border text-gray-300 hover:text-white hover:bg-naslos-border transition-colors"
                  on:click={() => copyURL(connectURL(share))}
                  title="Copy the share address"
                >{copied === connectURL(share) ? 'Copied' : 'Copy'}</button>
              </div>
            {:else if !share.enabled}
              <p class="text-xs text-gray-500 mt-2">Disabled — not reachable until enabled.</p>
            {/if}
          </div>
          <div class="flex gap-2">
            <button class="btn btn-secondary" on:click={() => editShare(share)}>Edit</button>
            <button class="btn btn-danger" on:click={() => deleteShare(share.name)}>Delete</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

{#if showForm}
  <ShareForm share={editingShare} on:close={() => { showForm = false; loadShares(); }} />
{/if}
