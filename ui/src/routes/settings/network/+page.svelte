<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  interface Resolver {
    address: string;
    protocol: string;
  }

  interface Pending {
    id: string;
    phase: string;
    link: string;
    target: string | null;
    deadline: string;
    error: string | null;
  }

  interface LinkConfig {
    addressing: string;
    addresses: string[];
    routes: { destination: string | null; gateway: string }[];
    mtu: number | null;
  }

  interface LinkView {
    name: string;
    kind: string;
    up: boolean;
    mac: string;
    mtu: number;
    speedMbps: number | null;
    duplex: string | null;
    addresses: { address: string; source: string }[];
    gateway: string | null;
    config: LinkConfig;
    features: { wol: string[]; ringRx: number | null; ringTx: number | null };
  }

  interface Inventory {
    hostname: { value: string; source: string };
    resolvers: {
      inherit: boolean;
      nameservers: Resolver[];
      searchDomains: string[];
      searchInherit: boolean;
      hostDns: { enabled: boolean; forwardKubeDnsToHost: boolean };
    };
    primaryLink: string;
    links: LinkView[];
    appliance: { domain: string; lanCidr: string };
    pending: Pending | null;
  }

  const emptyInventory: Inventory = {
    hostname: { value: '', source: 'auto-stable' },
    resolvers: {
      inherit: true,
      nameservers: [],
      searchDomains: [],
      searchInherit: true,
      hostDns: { enabled: false, forwardKubeDnsToHost: false }
    },
    primaryLink: '',
    links: [],
    appliance: { domain: '', lanCidr: '' },
    pending: null
  };

  let inventory: Inventory = emptyInventory;
  let loading = true;
  let saving = false;
  let message = '';
  let messageType = '';
  let nowMs = Date.now();

  // Link editor.
  let editing = '';
  let formAddressing = 'dhcp';
  let formAddresses = '';
  let formGateway = '';
  let formMtu = '';
  let formLowMtu = false;

  // Resolver editor.
  let nameserverMode = 'inherit';
  let nameserversText = '';
  let searchMode = 'inherit';
  let searchText = '';

  // Confirmation modal.
  let showConfirm = false;
  let confirmKind: 'link' | 'dns' = 'link';
  let confirmSummary = '';
  let confirmBody: Record<string, unknown> = {};

  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let clockTimer: ReturnType<typeof setInterval> | null = null;

  async function load() {
    try {
      const res = await fetch('/api/network');
      if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      inventory = await res.json();
      syncDnsForm();
      ensurePolling();
    } catch (e) {
      message = 'Failed to load the network inventory: ' + e;
      messageType = 'error';
    } finally {
      loading = false;
    }
  }

  function syncDnsForm() {
    nameserverMode = inventory.resolvers.inherit ? 'inherit' : 'explicit';
    nameserversText = inventory.resolvers.nameservers.map((ns) => ns.address).join(', ');
    searchMode = inventory.resolvers.searchInherit ? 'inherit' : 'explicit';
    searchText = inventory.resolvers.searchDomains.join(', ');
  }

  function ensurePolling() {
    if (!inventory.pending) {
      stopPolling();
      return;
    }
    if (pollTimer) {
      return;
    }
    pollTimer = setInterval(async () => {
      const id = inventory.pending ? inventory.pending.id : '';
      if (!id) {
        stopPolling();
        return;
      }
      try {
        const res = await fetch(`/api/network/changes/${id}`);
        if (!res.ok) {
          stopPolling();
          return;
        }
        const change = await res.json();
        const done = ['applied', 'reverted', 'failed'].includes(change.phase);
        inventory.pending = done ? null : change;
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

  function startEdit(link: LinkView) {
    editing = link.name;
    formAddressing = link.config.addressing === 'static' ? 'static' : 'dhcp';
    formAddresses = link.config.addresses.join(', ');
    formGateway = link.config.routes.length > 0 ? link.config.routes[0].gateway : '';
    formMtu = '';
    formLowMtu = false;
  }

  function cancelEdit() {
    editing = '';
  }

  function parseV4(value: string): number[] | null {
    const parts = value.trim().split('.');
    if (parts.length !== 4) return null;
    const nums = parts.map((part) => (/^\d{1,3}$/.test(part) ? Number(part) : NaN));
    if (nums.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return null;
    return nums;
  }

  function ipToInt(parts: number[]): number {
    return ((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]) >>> 0;
  }

  function cidrError(value: string): string | null {
    const [addr, prefixText, ...rest] = value.trim().split('/');
    if (!addr || prefixText === undefined || rest.length > 0) {
      return `"${value}" is not in CIDR form (for example 192.168.1.10/24)`;
    }
    if (!/^\d{1,3}$/.test(prefixText)) return `"${value}" has an invalid prefix length`;
    const prefix = Number(prefixText);
    const v4 = parseV4(addr);
    if (v4) {
      if (prefix < 1 || prefix > 32) return `"${value}" needs an IPv4 prefix between 1 and 32`;
      if (v4[0] === 0) return `"${value}" is unspecified`;
      if (v4[0] === 127) return `"${value}" is loopback`;
      if (v4[0] === 169 && v4[1] === 254) return `"${value}" is link-local`;
      if (v4[0] >= 224) return `"${value}" is multicast or reserved`;
      return null;
    }
    if (addr.includes(':')) {
      if (!/^[0-9a-fA-F:]+$/.test(addr)) return `"${value}" is not a valid IPv6 address`;
      if (prefix < 1 || prefix > 128) return `"${value}" needs an IPv6 prefix between 1 and 128`;
      return null;
    }
    return `"${value}" is not a valid address`;
  }

  function inSubnet(gateway: string, cidrs: string[]): boolean {
    const g = parseV4(gateway);
    if (!g) return false;
    const gNum = ipToInt(g);
    for (const cidr of cidrs) {
      const [addr, prefixText] = cidr.split('/');
      const a = parseV4(addr || '');
      if (!a || !/^\d+$/.test(prefixText || '')) continue;
      const prefix = Number(prefixText);
      if (prefix < 1 || prefix > 32) continue;
      const mask = prefix === 32 ? 0xffffffff : (0xffffffff << (32 - prefix)) >>> 0;
      if (((ipToInt(a) & mask) >>> 0) === ((gNum & mask) >>> 0)) return true;
    }
    return false;
  }

  function isIp(value: string): boolean {
    if (parseV4(value)) return true;
    return value.includes(':') && /^[0-9a-fA-F:]+$/.test(value);
  }

  function splitList(value: string): string[] {
    return value
      .split(/[\s,]+/)
      .map((part) => part.trim())
      .filter((part) => part !== '');
  }

  function validateLink(): string | null {
    if (formAddressing === 'static') {
      const addresses = splitList(formAddresses);
      if (addresses.length === 0 || addresses.length > 4) {
        return 'A static link needs 1-4 addresses.';
      }
      const seen = new Set<string>();
      for (const address of addresses) {
        const error = cidrError(address);
        if (error) return error;
        if (seen.has(address)) return `Duplicate address ${address}.`;
        seen.add(address);
      }
      if (formGateway.trim() !== '') {
        if (!isIp(formGateway.trim())) return `Gateway "${formGateway}" is not an IP address.`;
        if (parseV4(formGateway.trim())) {
          const v4Addresses = addresses.filter((a) => parseV4((a.split('/')[0] || '')));
          if (!inSubnet(formGateway.trim(), v4Addresses)) {
            return `Gateway ${formGateway} is not inside any configured subnet.`;
          }
        }
        if (addresses.some((a) => a.split('/')[0] === formGateway.trim())) {
          return `Gateway ${formGateway} is also a configured address.`;
        }
      }
    }
    if (formMtu.trim() !== '') {
      const mtu = Number(formMtu);
      if (!Number.isInteger(mtu) || mtu < 576 || mtu > 9216) {
        return 'MTU must be an integer between 576 and 9216.';
      }
      if (mtu < 1280 && !formLowMtu) {
        return 'An MTU below 1280 needs the extra confirmation checkbox.';
      }
    }
    return null;
  }

  function validateDns(): string | null {
    if (!inventory.primaryLink) {
      return 'No primary link was reported by Talos; a DNS change needs one.';
    }
    if (nameserverMode === 'explicit') {
      const servers = splitList(nameserversText);
      if (servers.length === 0 || servers.length > 3) {
        return 'Enter 1-3 DNS nameservers.';
      }
      for (const server of servers) {
        if (!isIp(server)) return `"${server}" is not an IP address.`;
      }
    }
    if (searchMode === 'explicit') {
      const domains = splitList(searchText);
      if (domains.length === 0 || domains.length > 6) {
        return 'Enter 1-6 search domains.';
      }
      for (const domain of domains) {
        if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/.test(domain.toLowerCase())) {
          return `"${domain}" is not a valid DNS name.`;
        }
      }
    }
    if (nameserverMode === 'inherit' && searchMode === 'inherit') {
      return 'Both DNS fields already inherit from DHCP; choose an explicit value or change the link instead.';
    }
    return null;
  }

  function requestLinkApply() {
    if (inventory.pending) return;
    const error = validateLink();
    if (error) {
      message = error;
      messageType = 'error';
      return;
    }
    const link = inventory.links.find((item) => item.name === editing);
    const addresses = splitList(formAddresses);
    const body: Record<string, unknown> = { link: editing, addressing: formAddressing };
    if (formAddressing === 'static') {
      body.addresses = addresses;
      if (formGateway.trim() !== '') body.gateway = formGateway.trim();
    }
    if (formMtu.trim() !== '') {
      body.mtu = Number(formMtu);
      if (Number(formMtu) < 1280) body.confirmLowMtu = true;
    }
    confirmKind = 'link';
    confirmBody = body;
    confirmSummary = `${link ? link.name : editing}: ${
      formAddressing === 'static' ? addresses.join(', ') : 'DHCP'
    }${formGateway.trim() ? ` via ${formGateway.trim()}` : ''}${
      formMtu.trim() ? `, MTU ${formMtu}` : ''
    }.`;
    showConfirm = true;
  }

  function requestDnsApply() {
    if (inventory.pending) return;
    const error = validateDns();
    if (error) {
      message = error;
      messageType = 'error';
      return;
    }
    const body: Record<string, unknown> = { link: inventory.primaryLink };
    body.nameservers =
      nameserverMode === 'explicit'
        ? { mode: 'explicit', servers: splitList(nameserversText) }
        : { mode: 'inherit' };
    body.searchDomains =
      searchMode === 'explicit'
        ? { mode: 'explicit', domains: splitList(searchText) }
        : { mode: 'inherit' };
    confirmKind = 'dns';
    confirmBody = body;
    confirmSummary = `DNS resolvers: ${
      nameserverMode === 'inherit' ? 'inherit from DHCP' : splitList(nameserversText).join(', ')
    }; search domains: ${
      searchMode === 'inherit' ? 'inherit from DHCP' : splitList(searchText).join(', ')
    }.`;
    showConfirm = true;
  }

  async function confirmApply() {
    showConfirm = false;
    saving = true;
    message = '';
    try {
      const res = await fetch('/api/network/changes', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(confirmBody)
      });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      message = 'Change started. The node is being reconfigured behind a 60 second guard.';
      messageType = 'success';
      editing = '';
      await load();
    } catch (e) {
      message = 'Could not start the change: ' + e;
      messageType = 'error';
    } finally {
      saving = false;
    }
  }

  async function revertNow() {
    const pending = inventory.pending;
    if (!pending) return;
    saving = true;
    try {
      const res = await fetch(`/api/network/changes/${pending.id}/revert`, { method: 'POST' });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      message = 'Reverting to the saved configuration.';
      messageType = 'success';
      await load();
    } catch (e) {
      message = 'Could not revert: ' + e;
      messageType = 'error';
    } finally {
      saving = false;
    }
  }

  function phaseLabel(phase: string): string {
    switch (phase) {
      case 'applying':
        return 'Applying the new configuration';
      case 'verifying':
        return 'Verifying the new address';
      case 'awaiting':
        return 'Confirmed, in the revert window';
      case 'applied':
        return 'Applied';
      case 'reverted':
        return 'Reverted';
      case 'failed':
        return 'Failed';
      default:
        return phase;
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
    stopPolling();
    if (clockTimer) clearInterval(clockTimer);
  });
</script>

<div class="max-w-4xl mx-auto">
  <div class="flex items-start justify-between gap-4 mb-2">
    <h1 class="text-3xl font-bold">Network</h1>
    <button class="btn btn-secondary" on:click={load} disabled={loading || saving}>Reload</button>
  </div>
  <p class="text-gray-400 mb-6">
    Host links, addressing, MTU and DNS. Every change is applied to the Talos machine configuration
    with a machine-side guard and an API watchdog.
  </p>

  {#if message}
    <div
      class={`p-4 rounded-lg border mb-6 ${messageType === 'success' ? 'bg-green-900/30 border-green-700 text-green-300' : 'bg-red-900/30 border-red-700 text-red-300'}`}
    >
      <span>{message}</span>
    </div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading network inventory...</p>
  {:else}
    {#if inventory.pending}
      <div class="card border-amber-700 space-y-3 mb-6">
        <div class="flex items-center justify-between gap-3">
          <div>
            <div class="font-bold text-amber-300">{phaseLabel(inventory.pending.phase)}</div>
            <div class="text-sm text-gray-400">
              {inventory.pending.link}
              {#if inventory.pending.target}→ {inventory.pending.target}{/if}
              {#if pendingSeconds > 0}· {pendingSeconds}s remaining{/if}
            </div>
            {#if inventory.pending.error}
              <div class="text-sm text-red-300 mt-1">{inventory.pending.error}</div>
            {/if}
          </div>
          <button class="btn btn-danger" on:click={revertNow} disabled={saving}>Revert now</button>
        </div>
        <p class="text-xs text-gray-500">
          A second change is blocked until this one finishes. A bad edit is reverted automatically by
          the node after 60 seconds if the new address never answers.
        </p>
      </div>
    {/if}

    <div class="card space-y-2 mb-6">
      <h2 class="text-lg font-bold">Hostname</h2>
      <div class="grid gap-2 sm:grid-cols-2 text-sm">
        <div>
          <div class="text-gray-400">Hostname</div>
          <div class="font-mono">{inventory.hostname.value || '—'}</div>
        </div>
        <div>
          <div class="text-gray-400">Source</div>
          <div>{inventory.hostname.source}</div>
        </div>
      </div>
      <p class="text-xs text-gray-500">
        The install path uses the stable auto hostname. Renaming the node is not offered here: it
        moves every local-path volume's node affinity and would strand installed apps.
      </p>
    </div>

    <div class="card space-y-4 mb-6">
      <h2 class="text-lg font-bold">DNS resolvers</h2>
      <div class="grid gap-4 sm:grid-cols-2">
        <div>
          <label class="label" for="network-ns-mode">Nameservers</label>
          <select id="network-ns-mode" bind:value={nameserverMode} class="input w-full">
            <option value="inherit">Inherit from DHCP</option>
            <option value="explicit">Explicit list</option>
          </select>
          {#if nameserverMode === 'explicit'}
            <input
              type="text"
              bind:value={nameserversText}
              class="input w-full mt-2"
              placeholder="1.1.1.1, 9.9.9.9"
            />
            <p class="text-xs text-gray-500 mt-1">1-3 addresses, comma or space separated.</p>
          {/if}
        </div>
        <div>
          <label class="label" for="network-search-mode">Search domains</label>
          <select id="network-search-mode" bind:value={searchMode} class="input w-full">
            <option value="inherit">Inherit from DHCP</option>
            <option value="explicit">Explicit list</option>
          </select>
          {#if searchMode === 'explicit'}
            <input type="text" bind:value={searchText} class="input w-full mt-2" placeholder="lan" />
            <p class="text-xs text-gray-500 mt-1">Up to 6 DNS names.</p>
          {/if}
        </div>
      </div>
      <div class="text-xs text-gray-500">
        Host DNS is
        <span class="font-mono">{inventory.resolvers.hostDns.enabled ? 'enabled' : 'disabled'}</span>
        (forwarding to the host:
        <span class="font-mono">{inventory.resolvers.hostDns.forwardKubeDnsToHost ? 'on' : 'off'}</span>).
        The appliance keeps the install-time host-DNS setting.
      </div>
      <div>
        <button
          class="btn btn-primary"
          on:click={requestDnsApply}
          disabled={saving || inventory.pending !== null}
        >
          Apply DNS
        </button>
      </div>
    </div>

    <div class="space-y-4 mb-6">
      <h2 class="text-lg font-bold">Links</h2>
      {#each inventory.links as link (link.name)}
        <div class="card space-y-3">
          <div class="flex flex-wrap items-center justify-between gap-2">
            <div>
              <div class="font-bold">
                {link.name}
                <span class="text-xs font-normal text-gray-400">({link.kind})</span>
              </div>
              <div class="text-sm text-gray-400">
                {link.up ? 'up' : 'down'} · MTU {link.mtu}
                {#if link.speedMbps}· {link.speedMbps} Mbps{/if}
                {#if link.duplex}· {link.duplex} duplex{/if}
              </div>
              <div class="text-xs text-gray-500 font-mono">{link.mac || 'no MAC'}</div>
            </div>
            <div class="text-sm text-right">
              <div>{link.config.addressing === 'static' ? 'Static' : 'DHCP'}</div>
              {#if link.gateway}
                <div class="text-gray-400">gateway {link.gateway}</div>
              {/if}
            </div>
          </div>

          <div class="text-sm">
            {#if link.addresses.length === 0}
              <span class="text-gray-500">No addresses</span>
            {:else}
              {#each link.addresses as address}
                <span class="inline-flex items-center gap-2 mr-3 font-mono">
                  {address.address}
                  <span class="text-xs text-gray-500">{address.source}</span>
                </span>
              {/each}
            {/if}
          </div>

          {#if link.features.wol.length > 0 || link.features.ringRx || link.features.ringTx}
            <div class="text-xs text-gray-500">
              {#if link.features.wol.length > 0}Wake-on-LAN: {link.features.wol.join(', ')}{/if}
              {#if link.features.ringRx || link.features.ringTx}
                · rings {link.features.ringRx ?? '—'}/{link.features.ringTx ?? '—'}
              {/if}
            </div>
          {/if}

          {#if editing === link.name}
            <div class="border-t border-naslos-dark pt-3 space-y-3">
              <div class="grid gap-3 sm:grid-cols-2">
                <div>
                  <label class="label" for={`network-addressing-${link.name}`}>Addressing</label>
                  <select
                    id={`network-addressing-${link.name}`}
                    bind:value={formAddressing}
                    class="input w-full"
                  >
                    <option value="dhcp">DHCP</option>
                    <option value="static">Static</option>
                  </select>
                </div>
                <div>
                  <label class="label" for={`network-mtu-${link.name}`}>MTU (optional)</label>
                  <input
                    id={`network-mtu-${link.name}`}
                    type="text"
                    inputmode="numeric"
                    bind:value={formMtu}
                    class="input w-full"
                    placeholder={`${link.mtu}`}
                  />
                </div>
              </div>
              {#if formAddressing === 'static'}
                <div>
                  <label class="label" for={`network-addresses-${link.name}`}>Addresses</label>
                  <input
                    id={`network-addresses-${link.name}`}
                    type="text"
                    bind:value={formAddresses}
                    class="input w-full"
                    placeholder="192.168.1.10/24"
                  />
                  <p class="text-xs text-gray-500 mt-1">1-4 CIDR addresses, comma separated.</p>
                </div>
                <div>
                  <label class="label" for={`network-gateway-${link.name}`}>Gateway</label>
                  <input
                    id={`network-gateway-${link.name}`}
                    type="text"
                    bind:value={formGateway}
                    class="input w-full"
                    placeholder="192.168.1.1"
                  />
                </div>
              {/if}
              {#if formMtu.trim() !== '' && Number(formMtu) < 1280}
                <label class="flex items-center gap-2 text-sm text-amber-300">
                  <input type="checkbox" bind:checked={formLowMtu} class="w-4 h-4 rounded" />
                  I understand an MTU below 1280 can break some traffic.
                </label>
              {/if}
              <p class="text-xs text-gray-500">
                A static address outside the kubelet's current subnet is added to
                <span class="font-mono">KubeNodeConfig.nodeIP.validSubnets</span> in the same change.
                Link speed and duplex cannot be set by Talos; they are shown only.
              </p>
              <div class="flex gap-3">
                <button
                  class="btn btn-primary"
                  on:click={requestLinkApply}
                  disabled={saving || inventory.pending !== null}
                >
                  Apply link
                </button>
                <button class="btn btn-secondary" on:click={cancelEdit} disabled={saving}>
                  Cancel
                </button>
              </div>
            </div>
          {:else}
            <div>
              <button
                class="btn btn-secondary"
                on:click={() => startEdit(link)}
                disabled={saving || inventory.pending !== null}
              >
                Edit {link.name}
              </button>
            </div>
          {/if}
        </div>
      {/each}
      {#if inventory.links.length === 0}
        <p class="text-gray-400">No physical links were reported by Talos.</p>
      {/if}
    </div>

    <div class="card space-y-2">
      <h2 class="text-lg font-bold">Appliance</h2>
      <div class="grid gap-2 sm:grid-cols-2 text-sm">
        <div>
          <div class="text-gray-400">Base domain</div>
          <div class="font-mono">{inventory.appliance.domain || 'not configured'}</div>
        </div>
        <div>
          <div class="text-gray-400">LAN CIDR</div>
          <div class="font-mono">{inventory.appliance.lanCidr || 'not configured'}</div>
        </div>
        <div>
          <div class="text-gray-400">Primary link</div>
          <div class="font-mono">{inventory.primaryLink || 'unknown'}</div>
        </div>
      </div>
      <p class="text-sm text-gray-400">
        Domain and certificate settings live in <a class="underline" href="/domains">Domains</a>;
        public records in <a class="underline" href="/dns">Dynamic DNS</a>.
      </p>
    </div>
  {/if}
</div>

{#if showConfirm}
  <div class="fixed inset-0 bg-black/60 flex items-center justify-center p-4 z-50" role="presentation">
    <div class="card max-w-lg w-full space-y-4" role="dialog" aria-modal="true" aria-labelledby="network-confirm-title">
      <h2 id="network-confirm-title" class="text-xl font-bold">Confirm the network change</h2>
      <p class="text-sm text-gray-300">{confirmSummary}</p>
      {#if confirmKind === 'link'}
        <div class="p-3 rounded-lg bg-amber-900/30 border border-amber-700 text-amber-200 text-sm space-y-2">
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
      <div class="flex gap-3">
        <button class="btn btn-primary" on:click={confirmApply} disabled={saving}>Apply</button>
        <button class="btn btn-secondary" on:click={() => (showConfirm = false)} disabled={saving}>
          Cancel
        </button>
      </div>
    </div>
  </div>
{/if}
