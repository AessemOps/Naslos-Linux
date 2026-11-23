<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '$lib/components/Icon.svelte';
  import LoadingState from '$lib/components/LoadingState.svelte';
  import Modal from '$lib/components/Modal.svelte';

  interface TokenRow {
    id: string;
    name: string;
    scope: string;
    createdBy: string;
    createdAt: string;
    expiresAt: string | null;
    lastUsedAt: string | null;
  }

  interface CallRow {
    tokenName: string;
    tool: string;
    scope: string;
    status: number;
    durationMs: number;
    at: string;
  }

  interface McpSettings {
    enabled: boolean;
    endpointUrl: string;
    tokens: TokenRow[];
    recentCalls: CallRow[];
  }

  const empty: McpSettings = {
    enabled: false,
    endpointUrl: '',
    tokens: [],
    recentCalls: []
  };

  let settings: McpSettings = empty;
  let loading = true;
  let loaded = false;
  let toggling = false;
  let error = '';

  // Create-token dialog.
  let showCreate = false;
  let formName = '';
  let formScope = 'read';
  let formExpires = '';
  let creating = false;
  let createError = '';

  // One-time reveal of the freshly minted token.
  let freshToken = '';
  let copied = false;

  // Revoke confirmation.
  let revokeTarget: TokenRow | null = null;
  let revoking = false;

  const scopeHelp: Record<string, string> = {
    read: 'Read-only tools (dashboards, listings, status).',
    write: 'Read tools plus changes (create shares, install apps, start VMs).',
    full:
      'Everything, including destructive tools. Acts as an administrator without 2FA; prefer an expiry.'
  };

  const expiryChoices = [
    { id: '', label: 'No expiry' },
    { id: '7', label: '7 days' },
    { id: '30', label: '30 days' },
    { id: '90', label: '90 days' },
    { id: '365', label: '1 year' }
  ];

  function fmtTime(s?: string | null): string {
    if (!s) return '—';
    const d = new Date(s);
    if (isNaN(d.getTime())) return s;
    return d.toLocaleString();
  }

  function fmtStatus(code: number): string {
    return code >= 400 ? `${code}` : `${code}`;
  }

  async function load() {
    loading = true;
    error = '';
    try {
      const res = await fetch('/api/mcp');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      settings = await res.json();
      loaded = true;
    } catch (e) {
      // The default (disabled, empty) state must never be editable: flipping
      // the toggle or revoking from a failed load would act on unknown data.
      error = 'Failed to load MCP settings: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      loading = false;
    }
  }

  async function setEnabled(enabled: boolean) {
    toggling = true;
    error = '';
    try {
      const res = await fetch('/api/mcp', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled })
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      settings = { ...settings, enabled };
    } catch (e) {
      error = 'Could not save the toggle: ' + e;
    } finally {
      toggling = false;
    }
  }

  function openCreate() {
    formName = '';
    formScope = 'read';
    formExpires = '';
    createError = '';
    showCreate = true;
  }

  async function createToken() {
    creating = true;
    createError = '';
    try {
      const payload: Record<string, unknown> = { name: formName.trim(), scope: formScope };
      if (formExpires) payload.expiresInDays = Number(formExpires);
      const res = await fetch('/api/mcp/tokens', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      const body = await res.json();
      if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
      freshToken = body.token;
      copied = false;
      showCreate = false;
      await load();
    } catch (e) {
      createError = String(e);
    } finally {
      creating = false;
    }
  }

  async function copyFresh() {
    try {
      await navigator.clipboard.writeText(freshToken);
      copied = true;
    } catch {
      // Clipboard access can be denied; the token stays selectable below.
      copied = false;
    }
  }

  async function revokeToken() {
    if (!revokeTarget) return;
    revoking = true;
    try {
      const res = await fetch(`/api/mcp/tokens/${encodeURIComponent(revokeTarget.id)}`, {
        method: 'DELETE'
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      revokeTarget = null;
      await load();
    } catch (e) {
      error = 'Could not revoke the token: ' + e;
      revokeTarget = null;
    } finally {
      revoking = false;
    }
  }

  function streamableSnippet(): string {
    const url = settings.endpointUrl || 'https://<your-naslos-domain>/mcp';
    return JSON.stringify(
      {
        mcpServers: {
          naslos: {
            type: 'http',
            url,
            headers: { Authorization: 'Bearer nmcp_YOUR_TOKEN' }
          }
        }
      },
      null,
      2
    );
  }

  function remoteSnippet(): string {
    const url = settings.endpointUrl || 'https://<your-naslos-domain>/mcp';
    return `npx mcp-remote ${url} --header "Authorization: Bearer nmcp_YOUR_TOKEN"`;
  }

  onMount(load);
</script>

<div class="max-w-4xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">MCP</h1>
  <p class="text-gray-400 mb-6">
    Let AI assistants and other MCP clients manage this NAS through scoped tools.
  </p>

  {#if loading}
    <LoadingState label="Loading MCP settings…" />
  {:else if !loaded}
    <div role="alert" class="card border-red-500/50 mb-6">
      <p class="text-sm text-red-300">{error || 'The MCP settings could not be loaded.'}</p>
      <button type="button" class="btn btn-secondary mt-3" on:click={load}>Retry</button>
    </div>
  {:else}
    {#if error}
      <div class="card border-red-500/50 mb-6 flex items-start gap-3">
        <div class="text-red-400 mt-0.5" aria-hidden="true"><Icon name="triangle-alert" /></div>
        <p class="text-sm text-red-300">{error}</p>
      </div>
    {/if}

    <!-- Endpoint toggle -->
    <div class="card mb-6">
      <div class="flex items-start justify-between gap-4">
        <div>
          <h2 class="font-bold">Endpoint</h2>
          <p class="text-sm text-gray-400 mt-1">
            Off by default. When enabled, MCP clients reach the tool endpoint at
            <span class="font-mono">{settings.endpointUrl || 'https://<your-domain>/mcp'}</span>.
          </p>
        </div>
        <button
          type="button"
          class="btn {settings.enabled ? 'btn-danger' : 'btn-primary'}"
          disabled={toggling}
          on:click={() => setEnabled(!settings.enabled)}
        >
          {toggling ? 'Saving…' : settings.enabled ? 'Disable' : 'Enable'}
        </button>
      </div>
      {#if settings.enabled}
        <div class="mt-4 flex items-start gap-3 rounded-lg bg-naslos-dark border border-naslos-border p-4">
          <div class="text-yellow-400 mt-0.5 shrink-0" aria-hidden="true">
            <Icon name="triangle-alert" />
          </div>
          <p class="text-sm text-gray-300">
            The endpoint is live. Tokens in the <strong>full</strong> scope act as an
            administrator <em>without two-factor authentication</em>: prefer
            <strong>read</strong> tokens for routine questions, add an expiry to anything
            stronger, and revoke tokens you no longer use. Disabling keeps the tokens but
            blocks every request.
          </p>
        </div>
      {/if}
    </div>

    <!-- Client configuration -->
    <div class="card mb-6">
      <h2 class="font-bold mb-1">Connect a client</h2>
      <p class="text-sm text-gray-400 mb-4">
        Create a token below, then paste it into the client's server configuration. The
        client must trust the certificate of this NAS's domain (a private CA needs to be
        installed on the client machine).
      </p>
      <p class="text-xs uppercase tracking-wide text-gray-500 mb-1">Streamable HTTP (Claude Desktop, Cursor, Kilo, …)</p>
      <pre class="text-xs bg-naslos-dark border border-naslos-border rounded-lg p-3 overflow-x-auto">{streamableSnippet()}</pre>
      <p class="text-xs uppercase tracking-wide text-gray-500 mb-1 mt-3">stdio-only clients (via mcp-remote)</p>
      <pre class="text-xs bg-naslos-dark border border-naslos-border rounded-lg p-3 overflow-x-auto">{remoteSnippet()}</pre>
    </div>

    <!-- Tokens -->
    <div class="card mb-6">
      <div class="flex items-center justify-between gap-4 mb-4">
        <div>
          <h2 class="font-bold">Tokens</h2>
          <p class="text-sm text-gray-400 mt-0.5">
            Each token carries one scope: read &lt; write &lt; full. Only the SHA-256 hash is
            stored; the value is shown once at creation.
          </p>
        </div>
        <button type="button" class="btn btn-primary" on:click={openCreate}>
          <span aria-hidden="true" class="inline-flex"><Icon name="plus" size={16} /></span>
          Create token
        </button>
      </div>

      {#if settings.tokens.length === 0}
        <p class="text-sm text-gray-400">No tokens yet.</p>
      {:else}
        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead>
              <tr class="text-left text-gray-400 border-b border-naslos-border">
                <th class="py-2 pr-4 font-medium">Name</th>
                <th class="py-2 pr-4 font-medium">Scope</th>
                <th class="py-2 pr-4 font-medium">Created</th>
                <th class="py-2 pr-4 font-medium">Expires</th>
                <th class="py-2 pr-4 font-medium">Last used</th>
                <th class="py-2 font-medium" aria-label="Actions"></th>
              </tr>
            </thead>
            <tbody>
              {#each settings.tokens as token (token.id)}
                <tr class="border-b border-naslos-border/50">
                  <td class="py-2 pr-4 font-medium">{token.name}</td>
                  <td class="py-2 pr-4"><span class="badge">{token.scope}</span></td>
                  <td class="py-2 pr-4 text-gray-400">{fmtTime(token.createdAt)}</td>
                  <td class="py-2 pr-4 text-gray-400">{token.expiresAt ? fmtTime(token.expiresAt) : 'never'}</td>
                  <td class="py-2 pr-4 text-gray-400">{fmtTime(token.lastUsedAt)}</td>
                  <td class="py-2 text-right">
                    <button
                      type="button"
                      class="btn btn-danger btn-sm"
                      on:click={() => (revokeTarget = token)}
                    >
                      Revoke
                    </button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </div>

    <!-- Recent calls -->
    <div class="card">
      <h2 class="font-bold mb-1">Recent calls</h2>
      <p class="text-sm text-gray-400 mb-4">
        The last tool calls per token, kept in memory (newest first, newest 50 across
        tokens). Arguments are never recorded.
      </p>
      {#if settings.recentCalls.length === 0}
        <p class="text-sm text-gray-400">No calls recorded yet.</p>
      {:else}
        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead>
              <tr class="text-left text-gray-400 border-b border-naslos-border">
                <th class="py-2 pr-4 font-medium">When</th>
                <th class="py-2 pr-4 font-medium">Token</th>
                <th class="py-2 pr-4 font-medium">Tool</th>
                <th class="py-2 pr-4 font-medium">Status</th>
                <th class="py-2 font-medium">Duration</th>
              </tr>
            </thead>
            <tbody>
              {#each settings.recentCalls as call, i (i)}
                <tr class="border-b border-naslos-border/50">
                  <td class="py-2 pr-4 text-gray-400">{fmtTime(call.at)}</td>
                  <td class="py-2 pr-4">{call.tokenName}</td>
                  <td class="py-2 pr-4 font-mono text-xs">{call.tool}</td>
                  <td class="py-2 pr-4 {call.status >= 400 ? 'text-red-400' : 'text-green-400'}">
                    {fmtStatus(call.status)}
                  </td>
                  <td class="py-2 text-gray-400">{call.durationMs} ms</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </div>
  {/if}
</div>

{#if showCreate}
  <Modal title="Create MCP token" onClose={() => (showCreate = false)}>
    <div class="space-y-4">
      <div>
        <label class="block text-sm font-medium mb-1" for="mcp-token-name">Name</label>
        <input
          id="mcp-token-name"
          class="input w-full"
          type="text"
          placeholder="e.g. kilo-laptop"
          bind:value={formName}
        />
        <p class="text-xs text-gray-500 mt-1">Letters, digits, spaces and - _ . only.</p>
      </div>
      <div>
        <span class="block text-sm font-medium mb-1">Scope</span>
        <div class="space-y-2">
          {#each ['read', 'write', 'full'] as scope (scope)}
            <label class="flex items-start gap-3">
              <input type="radio" class="mt-1" name="mcp-scope" value={scope} bind:group={formScope} />
              <span>
                <span class="font-medium">{scope}</span>
                <span class="block text-xs text-gray-400">{scopeHelp[scope]}</span>
              </span>
            </label>
          {/each}
        </div>
      </div>
      <div>
        <label class="block text-sm font-medium mb-1" for="mcp-token-expiry">Expires</label>
        <select id="mcp-token-expiry" class="input w-full" bind:value={formExpires}>
          {#each expiryChoices as choice (choice.id)}
            <option value={choice.id}>{choice.label}</option>
          {/each}
        </select>
        {#if formExpires === ''}
          <p class="text-xs text-yellow-400 mt-1">
            No expiry: the token works until revoked. Prefer an expiry for write/full tokens.
          </p>
        {/if}
      </div>
      {#if createError}
        <p class="text-sm text-red-300">{createError}</p>
      {/if}
    </div>
    <div slot="footer">
      <button type="button" class="btn" on:click={() => (showCreate = false)}>Cancel</button>
      <button
        type="button"
        class="btn btn-primary"
        disabled={creating || formName.trim() === ''}
        on:click={createToken}
      >
        {creating ? 'Creating…' : 'Create token'}
      </button>
    </div>
  </Modal>
{/if}

{#if freshToken}
  <Modal title="Token created" onClose={() => (freshToken = '')}>
    <div class="space-y-4">
      <div class="flex items-start gap-3 rounded-lg bg-naslos-dark border border-naslos-border p-4">
        <div class="text-yellow-400 mt-0.5 shrink-0" aria-hidden="true">
          <Icon name="triangle-alert" />
        </div>
        <p class="text-sm text-gray-300">
          Copy the token now. It is shown <strong>only this once</strong>: the NAS stores
          just its SHA-256 hash, so it cannot be displayed again.
        </p>
      </div>
      <div class="flex items-center gap-3">
        <code class="flex-1 text-xs bg-naslos-dark border border-naslos-border rounded-lg p-3 font-mono break-all select-all"
          >{freshToken}</code
        >
        <button type="button" class="btn" on:click={copyFresh}>
          {copied ? 'Copied' : 'Copy'}
        </button>
      </div>
    </div>
    <div slot="footer">
      <button type="button" class="btn btn-primary" on:click={() => (freshToken = '')}>
        Done
      </button>
    </div>
  </Modal>
{/if}

{#if revokeTarget}
  <Modal title="Revoke token" onClose={() => (revokeTarget = null)}>
    <p class="text-sm text-gray-300">
      Revoke <strong>{revokeTarget.name}</strong>? Clients using it stop working
      immediately. This cannot be undone (a replacement token must be created).
    </p>
    <div slot="footer">
      <button type="button" class="btn" on:click={() => (revokeTarget = null)}>Cancel</button>
      <button type="button" class="btn btn-danger" disabled={revoking} on:click={revokeToken}>
        {revoking ? 'Revoking…' : 'Revoke'}
      </button>
    </div>
  </Modal>
{/if}
