<script lang="ts">
  import { onMount } from 'svelte';

  export let domain: string;

  let status = 'loading';
  let reason = '';
  let secretName = '';

  async function load() {
    try {
      const res = await fetch(`/api/domains/${encodeURIComponent(domain)}/certificate`);
      const data = await res.json();
      status = data.status || 'unknown';
      reason = data.reason || '';
      secretName = data.secretName || '';
    } catch (e) {
      status = 'error';
      reason = String(e);
    }
  }

  function color(): string {
    switch (status) {
      case 'ready': return 'bg-green-900/50 text-green-400';
      case 'not-ready': return 'bg-red-900/50 text-red-400';
      case 'pending': case 'loading': return 'bg-yellow-900/50 text-yellow-400';
      case 'unavailable': return 'bg-gray-900/50 text-gray-400';
      default: return 'bg-gray-900/50 text-gray-400';
    }
  }

  onMount(load);
</script>

<span class={`text-xs px-2 py-0.5 rounded ${color()}`} title={reason || secretName}>
  {status}
</span>
