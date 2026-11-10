<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte';
  import type { AppPod, AppPods, AppView } from '$lib/appTypes';

  export let app: AppView;

  let pods: AppPod[] = [];
  let namespace = app.namespace;
  let pod = '';
  let container = '';
  let logEl: HTMLPreElement | null = null;
  let socket: WebSocket | null = null;
  let connected = false;
  let loading = true;
  let error = '';
  let follow = true;
  let buffer = '';

  // Keep the pane bounded: a long-followed stream must not grow the tab.
  const MAX_CHARS = 200_000;

  $: containers = pods.find((p) => p.name === pod)?.containers ?? [];

  async function loadPods() {
    loading = true;
    error = '';
    try {
      const res = await fetch(`/api/apps/${encodeURIComponent(app.name)}/pods`);
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      const report = data as AppPods;
      namespace = report.namespace || app.namespace;
      pods = report.pods ?? [];
      if (!pods.some((p) => p.name === pod)) {
        pod = pods[0]?.name ?? '';
        container = pods[0]?.containers[0]?.name ?? '';
      } else if (!containers.some((c) => c.name === container)) {
        container = containers[0]?.name ?? '';
      }
      if (pod) connect();
      else closeSocket();
    } catch (e) {
      error = `Cannot list pods: ${e}`;
      pods = [];
      closeSocket();
    } finally {
      loading = false;
    }
  }

  function closeSocket() {
    if (!socket) return;
    const stale = socket;
    socket = null;
    connected = false;
    stale.onopen = null;
    stale.onmessage = null;
    stale.onerror = null;
    stale.onclose = null;
    if (stale.readyState === WebSocket.OPEN || stale.readyState === WebSocket.CONNECTING) {
      stale.close();
    }
  }

  // The stream is plain kubectl output; only the wrapper's error lines carry
  // ANSI colour codes, which would otherwise show as escape gibberish.
  function stripAnsi(text: string): string {
    return text.replace(/\u001b\[[0-9;?]*[a-zA-Z]/g, '');
  }

  async function append(text: string) {
    buffer += stripAnsi(text);
    if (buffer.length > MAX_CHARS) buffer = buffer.slice(buffer.length - MAX_CHARS);
    if (follow) {
      await tick();
      if (logEl) logEl.scrollTop = logEl.scrollHeight;
    }
  }

  function connect() {
    closeSocket();
    if (!pod) return;
    buffer = '';
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const query = new URLSearchParams({ namespace, pod });
    if (container) query.set('container', container);
    const ws = new WebSocket(`${proto}//${window.location.host}/api/ws/logs?${query.toString()}`);
    socket = ws;
    ws.onopen = () => {
      if (socket !== ws) return;
      connected = true;
      error = '';
    };
    ws.onmessage = (event) => {
      if (socket === ws) void append(String(event.data));
    };
    ws.onerror = () => {
      if (socket !== ws) return;
      error = 'The log stream failed.';
      connected = false;
    };
    ws.onclose = () => {
      if (socket !== ws) return;
      connected = false;
      socket = null;
    };
  }

  function selectPod(event: Event) {
    pod = (event.currentTarget as HTMLSelectElement).value;
    container = pods.find((p) => p.name === pod)?.containers[0]?.name ?? '';
    connect();
  }

  function selectContainer(event: Event) {
    container = (event.currentTarget as HTMLSelectElement).value;
    connect();
  }

  // Scrolling up pauses follow; scrolling back to the bottom resumes it.
  function onScroll() {
    if (!logEl) return;
    follow = logEl.scrollTop + logEl.clientHeight >= logEl.scrollHeight - 24;
  }

  onMount(loadPods);
  onDestroy(closeSocket);
</script>

<section>
  <div class="flex items-center justify-between mb-2">
    <h3 class="text-xs uppercase tracking-wide text-gray-500">Logs</h3>
    <div class="flex items-center gap-3 text-xs">
      <span class={connected ? 'text-green-400' : 'text-gray-500'} role="status">
        {connected ? 'live' : 'offline'}
      </span>
      <label class="flex items-center gap-1 text-gray-400">
        <input type="checkbox" bind:checked={follow} /> follow
      </label>
      <button
        type="button"
        class="text-gray-400 hover:text-white"
        title="Reload pods and reconnect"
        aria-label="Reload pods and reconnect"
        on:click={loadPods}>↻</button
      >
    </div>
  </div>

  {#if loading}
    <p class="text-xs text-gray-500">Loading pods…</p>
  {:else if pods.length === 0}
    <p class="text-xs text-gray-500">No pods are running for this app.</p>
  {:else}
    <div class="flex gap-2 mb-2">
      <select class="input text-xs flex-1 min-w-0" value={pod} on:change={selectPod} aria-label="Pod">
        {#each pods as p (p.name)}
          <option value={p.name}>{p.name} · {p.phase}</option>
        {/each}
      </select>
      {#if containers.length > 1}
        <select
          class="input text-xs w-32"
          value={container}
          on:change={selectContainer}
          aria-label="Container"
        >
          {#each containers as c (c.name)}
            <option value={c.name}>{c.name}</option>
          {/each}
        </select>
      {/if}
    </div>
    <pre
      bind:this={logEl}
      on:scroll={onScroll}
      class="h-64 overflow-auto rounded-lg bg-black/60 border border-naslos-border p-2 text-[11px] leading-4 font-mono whitespace-pre-wrap break-words"
      aria-label="Live application logs">{buffer}</pre
    >
    {#if error}
      <p class="text-xs text-red-300 mt-1" role="alert">{error}</p>
    {/if}
  {/if}
</section>
