<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import StatusBadge from './StatusBadge.svelte';

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
      const res = await fetch(`/api/domains/${encodeURIComponent(domain)}/certificate`, {
        cache: 'no-store'
      });
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
</script>

<span title={reason || secretName}>
  <StatusBadge state={status} />
</span>
