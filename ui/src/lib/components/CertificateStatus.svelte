<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  export let domain: string;

  let status = 'loading';
  let reason = '';
  let secretName = '';
  let timer: ReturnType<typeof setTimeout> | null = null;
  let destroyed = false;
  let seq = 0;
  let delay = 10_000;

  async function load() {
    const current = ++seq;
    try {
      const res = await fetch(
        `/api/domains/${encodeURIComponent(domain)}/certificate`,
        { cache: 'no-store' }
      );
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      if (destroyed || current !== seq) return;
      status = data.status || 'unknown';
      reason = data.reason || '';
      secretName = data.secretName || '';
    } catch (e) {
      if (destroyed || current !== seq) return;
      status = 'error';
      reason = String(e);
    }
    if (destroyed || current !== seq) return;
    // A certificate can take a while to issue (ACME order, DNS propagation),
    // and a not-ready one can recover after a DNS fix. Keep polling, but back
    // off up to five minutes so a broken setup does not hammer the API; a
    // ready certificate is rechecked slowly for renewal/removal.
    if (status === 'ready') {
      delay = 60_000;
    } else if (status === 'not-ready' || status === 'error' || status === 'unavailable') {
      delay = Math.min(delay * 2, 300_000);
    } else {
      delay = 10_000;
    }
    schedule();
  }

  function schedule() {
    if (destroyed) return;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void load();
    }, delay);
  }

  onMount(() => {
    void load();
  });

  onDestroy(() => {
    destroyed = true;
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  });

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
