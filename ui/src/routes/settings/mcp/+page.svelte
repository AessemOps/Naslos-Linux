<script lang="ts">
  import { onMount } from 'svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import type { Tone } from '$lib/status';
  import { api } from '$lib/api';
  import { fmtTime } from '$lib/format';
  import { confirmAction } from '$lib/stores/confirm';

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

  let settings: McpSettings = empty;
  let loading = true;
  let loaded = false;
  let toggling = false;
  let error = '';
  let actionError = '';

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

  let revoking = '';

  function scopeTone(scope: string): Tone {
    if (scope === 'full') return 'warn';
    if (scope === 'write') return 'info';
    return 'neutral';
  }

  function statusTone(code: number): Tone {
    if (code >= 400) return 'bad';
    if (code >= 300) return 'warn';
    return 'ok';
  }

  async function load() {
    loading = true;
    error = '';
    try {
      settings = await api<McpSettings>('/api/mcp');
      loaded = true;
    } catch (e) {
      // The default (disabled, empty) state must never be editable: flipping
      // the toggle or revoking from a failed load would act on unknown data.
      error = 'Failed to load MCP settings: ' + (e instanceof Error ? e.message : String(e));
      loaded = false;
    } finally {
      loading = false;
    }
  }

  function retryLoad() {
    load();
  }

  async function setEnabled(enabled: boolean) {
    toggling = true;
    actionError = '';
    try {
      await api('/api/mcp', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled })
      });
      settings = { ...settings, enabled };
    } catch (e) {
      actionError = 'Could not save the toggle: ' + (e instanceof Error ? e.message : String(e));
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
    if (formName.trim() === '') return;
    creating = true;
    createError = '';
    try {
      const payload: Record<string, unknown> = { name: formName.trim(), scope: formScope };
      if (formExpires) payload.expiresInDays = Number(formExpires);
      const body = await api<{ token?: string }>('/api/mcp/tokens', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      freshToken = body.token ?? '';
      copied = false;
      showCreate = false;
      await load();
    } catch (e) {
      createError = e instanceof Error ? e.message : String(e);
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

  async function revokeToken(token: TokenRow) {
    const ok = await confirmAction({
      title: 'Revoke token',
      message: `Revoke “${token.name}”? Clients using it stop working immediately. This cannot be undone (a replacement token must be created).`,
      confirmLabel: 'Revoke',
      tone: 'danger'
    });
    if (!ok) return;
    revoking = token.id;
    actionError = '';
    try {
      await api(`/api/mcp/tokens/${encodeURIComponent(token.id)}`, { method: 'DELETE' });
      await load();
    } catch (e) {
      actionError = 'Could not revoke the token: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      revoking = '';
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

<div class="mx-auto max-w-4xl">
  <PageHeader
    title="MCP"
    subtitle="Let AI assistants and other MCP clients manage this NAS through scoped tools."
  />

  <AsyncState
    loading={loading}
    error={loaded ? '' : error}
    onRetry={retryLoad}
    loadingText="Loading MCP settings…"
  >
    {#if actionError}
      <div class="callout callout-bad" role="alert">{actionError}</div>
    {/if}

    <section class="card mb-6" aria-labelledby="mcp-endpoint">
      <div class="flex flex-wrap items-start justify-between gap-4">
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-2">
            <h2 id="mcp-endpoint" class="font-bold">Endpoint</h2>
            <StatusBadge
              label={settings.enabled ? 'enabled' : 'disabled'}
              tone={settings.enabled ? 'ok' : 'neutral'}
            />
          </div>
          <p class="mt-1 text-sm text-gray-400">
            Off by default. When enabled, MCP clients reach the tool endpoint at
            <span class="font-mono">{settings.endpointUrl || 'https://<your-domain>/mcp'}</span>.
          </p>
        </div>
        <Button
          variant={settings.enabled ? 'danger' : 'primary'}
          loading={toggling}
          disabled={toggling}
          on:click={() => setEnabled(!settings.enabled)}
        >
          {settings.enabled ? 'Disable' : 'Enable'}
        </Button>
      </div>
      {#if settings.enabled}
        <div class="callout callout-warn mt-4 flex items-start gap-3">
          <div class="mt-0.5 shrink-0" aria-hidden="true">
            <Icon name="triangle-alert" />
          </div>
          <p class="text-sm">
            The endpoint is live. Tokens in the <strong>full</strong> scope act as an
            administrator <em>without two-factor authentication</em>: prefer
            <strong>read</strong> tokens for routine questions, add an expiry to anything
            stronger, and revoke tokens you no longer use. Disabling keeps the tokens but
            blocks every request.
          </p>
        </div>
      {/if}
    </section>

    <section class="card mb-6" aria-labelledby="mcp-client">
      <h2 id="mcp-client" class="mb-1 font-bold">Connect a client</h2>
      <p class="mb-4 text-sm text-gray-400">
        Create a token below, then paste it into the client's server configuration. The
        client must trust the certificate of this NAS's domain (a private CA needs to be
        installed on the client machine).
      </p>
      <p class="mb-1 text-xs uppercase tracking-wide text-gray-500">
        Streamable HTTP (Claude Desktop, Cursor, Kilo, …)
      </p>
      <pre class="overflow-x-auto rounded-lg border border-naslos-border bg-naslos-dark p-3 font-mono text-xs">{streamableSnippet()}</pre>
      <p class="mb-1 mt-3 text-xs uppercase tracking-wide text-gray-500">
        stdio-only clients (via mcp-remote)
      </p>
      <pre class="overflow-x-auto rounded-lg border border-naslos-border bg-naslos-dark p-3 font-mono text-xs">{remoteSnippet()}</pre>
    </section>

    <section class="card mb-6" aria-labelledby="mcp-tokens">
      <div class="mb-4 flex flex-wrap items-start justify-between gap-4">
        <div class="w-full min-w-0 sm:w-auto sm:flex-1">
          <h2 id="mcp-tokens" class="font-bold">Tokens</h2>
          <p class="mt-0.5 text-sm text-gray-400">
            Each token carries one scope: read &lt; write &lt; full. Only the SHA-256 hash is
            stored; the value is shown once at creation.
          </p>
        </div>
        <Button variant="primary" on:click={openCreate}>
          <span aria-hidden="true" class="inline-flex"><Icon name="plus" size={16} /></span>
          Create token
        </Button>
      </div>

      {#if settings.tokens.length === 0}
        <p class="text-sm text-gray-400">No tokens yet.</p>
      {:else}
        <DataTable>
          <table>
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Scope</th>
                <th scope="col" class="hidden md:table-cell">Created</th>
                <th scope="col" class="hidden md:table-cell">Expires</th>
                <th scope="col" class="hidden md:table-cell">Last used</th>
                <th scope="col" aria-label="Actions"></th>
              </tr>
            </thead>
            <tbody>
              {#each settings.tokens as token (token.id)}
                <tr>
                  <td class="font-medium">{token.name}</td>
                  <td><StatusBadge label={token.scope} tone={scopeTone(token.scope)} /></td>
                  <td class="hidden text-gray-400 md:table-cell">{fmtTime(token.createdAt)}</td>
                  <td class="hidden text-gray-400 md:table-cell">
                    {token.expiresAt ? fmtTime(token.expiresAt) : 'never'}
                  </td>
                  <td class="hidden text-gray-400 md:table-cell">{fmtTime(token.lastUsedAt)}</td>
                  <td class="text-right">
                    <Button
                      variant="danger"
                      size="sm"
                      loading={revoking === token.id}
                      on:click={() => revokeToken(token)}
                    >Revoke</Button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </DataTable>
      {/if}
    </section>

    <section class="card" aria-labelledby="mcp-calls">
      <h2 id="mcp-calls" class="mb-1 font-bold">Recent calls</h2>
      <p class="mb-4 text-sm text-gray-400">
        The last tool calls per token, kept in memory (newest first, newest 50 across
        tokens). Arguments are never recorded.
      </p>
      {#if settings.recentCalls.length === 0}
        <p class="text-sm text-gray-400">No calls recorded yet.</p>
      {:else}
        <DataTable>
          <table>
            <thead>
              <tr>
                <th scope="col" class="hidden md:table-cell">When</th>
                <th scope="col" class="hidden md:table-cell">Token</th>
                <th scope="col">Tool</th>
                <th scope="col">Status</th>
                <th scope="col" class="hidden md:table-cell">Duration</th>
              </tr>
            </thead>
            <tbody>
              {#each settings.recentCalls as call, i (i)}
                <tr>
                  <td class="hidden text-gray-400 md:table-cell">{fmtTime(call.at)}</td>
                  <td class="hidden md:table-cell">{call.tokenName}</td>
                  <td class="break-all font-mono text-xs">{call.tool}</td>
                  <td><StatusBadge label={String(call.status)} tone={statusTone(call.status)} /></td>
                  <td class="hidden text-right font-mono text-xs text-gray-400 md:table-cell">{call.durationMs} ms</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </DataTable>
      {/if}
    </section>
  </AsyncState>
</div>

{#if showCreate}
  <Modal title="Create MCP token" onClose={() => (showCreate = false)}>
    <form id="mcp-create-form" class="space-y-4" on:submit|preventDefault={createToken}>
      <FormField label="Name" id="mcp-token-name">
        <input
          id="mcp-token-name"
          class="input w-full"
          type="text"
          placeholder="e.g. kilo-laptop"
          bind:value={formName}
        />
        <p class="mt-1 text-xs text-gray-500">Letters, digits, spaces and - _ . only.</p>
      </FormField>

      <fieldset>
        <legend class="label">Scope</legend>
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
      </fieldset>

      <FormField label="Expires" id="mcp-token-expiry">
        <select id="mcp-token-expiry" class="input w-full" bind:value={formExpires}>
          {#each expiryChoices as choice (choice.id)}
            <option value={choice.id}>{choice.label}</option>
          {/each}
        </select>
        {#if formExpires === ''}
          <p class="mt-1 text-xs text-amber-300">
            No expiry: the token works until revoked. Prefer an expiry for write/full tokens.
          </p>
        {/if}
      </FormField>

      {#if createError}
        <p role="alert" class="text-sm text-red-300">{createError}</p>
      {/if}
    </form>
    <div slot="footer">
      <Button variant="secondary" on:click={() => (showCreate = false)}>Cancel</Button>
      <Button
        type="submit"
        form="mcp-create-form"
        variant="primary"
        loading={creating}
        disabled={formName.trim() === ''}
      >Create token</Button>
    </div>
  </Modal>
{/if}

{#if freshToken}
  <Modal title="Token created" onClose={() => (freshToken = '')}>
    <div class="space-y-4">
      <div class="callout callout-warn flex items-start gap-3">
        <div class="mt-0.5 shrink-0" aria-hidden="true">
          <Icon name="triangle-alert" />
        </div>
        <p class="text-sm">
          Copy the token now. It is shown <strong>only this once</strong>: the NAS stores
          just its SHA-256 hash, so it cannot be displayed again.
        </p>
      </div>
      <div class="flex flex-wrap items-center gap-3">
        <code class="min-w-0 flex-1 break-all rounded-lg border border-naslos-border bg-naslos-dark p-3 font-mono text-xs select-all">{freshToken}</code>
        <Button variant="secondary" on:click={copyFresh}>{copied ? 'Copied' : 'Copy'}</Button>
      </div>
    </div>
    <div slot="footer">
      <Button variant="primary" on:click={() => (freshToken = '')}>Done</Button>
    </div>
  </Modal>
{/if}
