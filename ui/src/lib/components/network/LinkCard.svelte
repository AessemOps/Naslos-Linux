<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from '../Button.svelte';
  import FormField from '../FormField.svelte';
  import type { LinkView } from './network';
  import { splitList, validateLink } from './network';

  export let link: LinkView;
  export let disabled = false;
  export let saving = false;

  const dispatch = createEventDispatcher<{
    apply: { body: Record<string, unknown>; summary: string };
  }>();

  let editing = false;
  let addressing = 'dhcp';
  let addresses = '';
  let gateway = '';
  let mtu = '';
  let lowMtu = false;
  let formError = '';

  function startEdit() {
    editing = true;
    addressing = link.config.addressing === 'static' ? 'static' : 'dhcp';
    addresses = link.config.addresses.join(', ');
    gateway = link.config.routes.length > 0 ? link.config.routes[0].gateway : '';
    mtu = '';
    lowMtu = false;
    formError = '';
  }

  function cancelEdit() {
    editing = false;
    formError = '';
  }

  function apply() {
    if (disabled || saving) return;
    const error = validateLink({ addressing, addresses, gateway, mtu, lowMtu });
    if (error) {
      formError = error;
      return;
    }
    formError = '';
    const body: Record<string, unknown> = { link: link.name, addressing };
    if (addressing === 'static') {
      body.addresses = splitList(addresses);
      if (gateway.trim() !== '') body.gateway = gateway.trim();
    }
    if (mtu.trim() !== '') {
      body.mtu = Number(mtu);
      if (Number(mtu) < 1280) body.confirmLowMtu = true;
    }
    const summary = `${link.name}: ${
      addressing === 'static' ? splitList(addresses).join(', ') : 'DHCP'
    }${gateway.trim() ? ` via ${gateway.trim()}` : ''}${mtu.trim() ? `, MTU ${mtu}` : ''}.`;
    editing = false;
    dispatch('apply', { body, summary });
  }
</script>

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
      <div class="font-mono text-xs text-gray-500">{link.mac || 'no MAC'}</div>
    </div>
    <div class="text-sm sm:text-right">
      <div>{link.config.addressing === 'static' ? 'Static' : 'DHCP'}</div>
      {#if link.gateway}
        <div class="text-gray-400">gateway <span class="font-mono">{link.gateway}</span></div>
      {/if}
    </div>
  </div>

  <div class="flex flex-wrap gap-x-3 gap-y-1 text-sm">
    {#if link.addresses.length === 0}
      <span class="text-gray-500">No addresses</span>
    {:else}
      {#each link.addresses as address (address.address)}
        <span class="inline-flex items-baseline gap-2">
          <span class="font-mono">{address.address}</span>
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

  {#if editing}
    <form class="space-y-3 border-t border-naslos-border pt-3" on:submit|preventDefault={apply}>
      <div class="grid gap-3 sm:grid-cols-2">
        <FormField label="Addressing" id={`network-addressing-${link.name}`}>
          <select
            id={`network-addressing-${link.name}`}
            bind:value={addressing}
            class="input w-full"
            {disabled}
          >
            <option value="dhcp">DHCP</option>
            <option value="static">Static</option>
          </select>
        </FormField>
        <FormField label="MTU (optional)" id={`network-mtu-${link.name}`}>
          <input
            id={`network-mtu-${link.name}`}
            type="text"
            inputmode="numeric"
            bind:value={mtu}
            class="input w-full"
            placeholder={`${link.mtu}`}
            {disabled}
          />
        </FormField>
      </div>

      {#if addressing === 'static'}
        <FormField label="Addresses" id={`network-addresses-${link.name}`}>
          <input
            id={`network-addresses-${link.name}`}
            type="text"
            bind:value={addresses}
            class="input w-full"
            placeholder="192.168.1.10/24"
            {disabled}
          />
          <p class="mt-1 text-xs text-gray-500">1-4 CIDR addresses, comma separated.</p>
        </FormField>
        <FormField label="Gateway" id={`network-gateway-${link.name}`}>
          <input
            id={`network-gateway-${link.name}`}
            type="text"
            bind:value={gateway}
            class="input w-full"
            placeholder="192.168.1.1"
            {disabled}
          />
        </FormField>
      {/if}

      {#if mtu.trim() !== '' && Number(mtu) < 1280}
        <label class="flex items-center gap-2 text-sm text-amber-300">
          <input type="checkbox" bind:checked={lowMtu} class="h-4 w-4 rounded" {disabled} />
          I understand an MTU below 1280 can break some traffic.
        </label>
      {/if}

      <p class="text-xs text-gray-500">
        A static address outside the kubelet's current subnet is added to
        <span class="font-mono">KubeNodeConfig.nodeIP.validSubnets</span> in the same change.
        Link speed and duplex cannot be set by Talos; they are shown only.
      </p>

      {#if formError}
        <div class="callout callout-bad" role="alert">{formError}</div>
      {/if}

      <div class="flex flex-wrap gap-3">
        <Button type="submit" variant="primary" loading={saving} {disabled}>Apply link</Button>
        <Button variant="secondary" {disabled} on:click={cancelEdit}>Cancel</Button>
      </div>
    </form>
  {:else}
    <div>
      <Button variant="secondary" {disabled} on:click={startEdit}>Edit {link.name}</Button>
    </div>
  {/if}
</div>
