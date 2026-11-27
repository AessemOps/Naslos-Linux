<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from '../Button.svelte';
  import FormField from '../FormField.svelte';
  import type { Resolvers } from './network';
  import { splitList, validateDns } from './network';

  export let resolvers: Resolvers;
  export let primaryLink = '';
  export let disabled = false;
  export let saving = false;

  const dispatch = createEventDispatcher<{
    apply: { body: Record<string, unknown>; summary: string };
  }>();

  let nameserverMode = 'inherit';
  let nameserversText = '';
  let searchMode = 'inherit';
  let searchText = '';
  let formError = '';
  let previous: Resolvers | null = null;

  // Re-sync after every inventory reload without remounting the card.
  $: if (resolvers !== previous) {
    previous = resolvers;
    nameserverMode = resolvers.inherit ? 'inherit' : 'explicit';
    nameserversText = resolvers.nameservers.map((ns) => ns.address).join(', ');
    searchMode = resolvers.searchInherit ? 'inherit' : 'explicit';
    searchText = resolvers.searchDomains.join(', ');
    formError = '';
  }

  function apply() {
    const error = validateDns(primaryLink, nameserverMode, nameserversText, searchMode, searchText);
    if (error) {
      formError = error;
      return;
    }
    formError = '';
    const body: Record<string, unknown> = { link: primaryLink };
    body.nameservers =
      nameserverMode === 'explicit'
        ? { mode: 'explicit', servers: splitList(nameserversText) }
        : { mode: 'inherit' };
    body.searchDomains =
      searchMode === 'explicit'
        ? { mode: 'explicit', domains: splitList(searchText) }
        : { mode: 'inherit' };
    const summary = `DNS resolvers: ${
      nameserverMode === 'inherit' ? 'inherit from DHCP' : splitList(nameserversText).join(', ')
    }; search domains: ${
      searchMode === 'inherit' ? 'inherit from DHCP' : splitList(searchText).join(', ')
    }.`;
    dispatch('apply', { body, summary });
  }
</script>

<section class="card mb-6 space-y-4" aria-labelledby="network-resolvers">
  <h2 id="network-resolvers" class="text-lg font-bold">DNS resolvers</h2>

  <form class="space-y-4" on:submit|preventDefault={apply}>
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <FormField label="Nameservers" id="network-ns-mode">
          <select id="network-ns-mode" bind:value={nameserverMode} class="input w-full" {disabled}>
            <option value="inherit">Inherit from DHCP</option>
            <option value="explicit">Explicit list</option>
          </select>
        </FormField>
        {#if nameserverMode === 'explicit'}
          <div class="mt-2">
            <FormField label="Nameserver addresses" id="network-ns-list">
              <input
                id="network-ns-list"
                type="text"
                bind:value={nameserversText}
                class="input w-full"
                placeholder="1.1.1.1, 9.9.9.9"
                {disabled}
              />
            </FormField>
            <p class="mt-1 text-xs text-gray-500">1-3 addresses, comma or space separated.</p>
          </div>
        {/if}
      </div>
      <div>
        <FormField label="Search domains" id="network-search-mode">
          <select id="network-search-mode" bind:value={searchMode} class="input w-full" {disabled}>
            <option value="inherit">Inherit from DHCP</option>
            <option value="explicit">Explicit list</option>
          </select>
        </FormField>
        {#if searchMode === 'explicit'}
          <div class="mt-2">
            <FormField label="Search domain list" id="network-search-list">
              <input
                id="network-search-list"
                type="text"
                bind:value={searchText}
                class="input w-full"
                placeholder="lan.example.com"
                {disabled}
              />
            </FormField>
            <p class="mt-1 text-xs text-gray-500">Up to 6 DNS names.</p>
          </div>
        {/if}
      </div>
    </div>

    <p class="text-xs text-gray-500">
      Host DNS is
      <span class="font-mono">{resolvers.hostDns.enabled ? 'enabled' : 'disabled'}</span>
      (forwarding to the host:
      <span class="font-mono">{resolvers.hostDns.forwardKubeDnsToHost ? 'on' : 'off'}</span>).
      The appliance keeps the install-time host-DNS setting.
    </p>

    {#if formError}
      <div class="callout callout-bad" role="alert">{formError}</div>
    {/if}

    <div>
      <Button type="submit" variant="primary" loading={saving} {disabled}>Apply DNS</Button>
    </div>
  </form>
</section>
