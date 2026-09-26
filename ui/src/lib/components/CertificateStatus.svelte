<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  export let domain: string;

  let status = 'loading';
  let reason = '';
  let secretName = '';
  let timer: ReturnType<typeof setInterval> | null = null;

  async function load() {
    try {
      const res = await fetch(
        `/api/domains/${encodeURIComponent(domain)}/certificate`,
        { cache: 'no-store' }
      );
      const data = await res.json();
      status = data.status || 'unknown';
      reason = data.reason || '';
      secretName = data.secretName || '';
    } catch (e) {
      status = 'error';
      reason = String(e);
    }
    // A certificate can take a while to issue (ACME order, DNS propagation), so
    // poll until the status settles instead of showing the initial "pending"
    // until the operator reloads the page. no-store above keeps a cached
    // response from pinning a stale badge.
    if (status !== 'loading' && status !== 'pending') {
      stop();
    }
  }

  function stop() {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
  }

  onMount(() => {
    load();
    timer = setInterval(load, 10_000);
  });

  onDestroy(stop);

  function color(s: string): string {
    switch (s) {
      case 'ready': return 'bg-green-900/50 text-green-400';
      case 'not-ready': return 'bg-red-900/50 text-red-400';
      case 'pending': case 'loading': return 'bg-yellow-900/50 text-yellow-400';
      case 'unavailable': return 'bg-gray-900/50 text-gray-400';
      default: return 'bg-gray-900/50 text-gray-400';
    }
  }

  // Svelte 5 does not track state read inside a function called from the
  // template, so `class={color()}` would keep the class from the first render
  // (yellow "loading") while the text updated. Pass the state in so the
  // reactive statement has an explicit dependency.
  $: badgeClass = color(status);
</script>

<span class={`text-xs px-2 py-0.5 rounded ${badgeClass}`} title={reason || secretName}>
  {status}
</span>
