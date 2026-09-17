<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { Terminal } from 'xterm';
  import { FitAddon } from 'xterm-addon-fit';
  import 'xterm/css/xterm.css';

  // Shell names mirror the API's allowlist (api/internal/server/websocket.go):
  // the terminal cannot run an arbitrary command, only a shell.
  const SHELLS = ['bash', 'sh', 'ash', 'zsh'];

  interface Pod {
    name: string;
    namespace: string;
    containers: string[];
    phase: string;
    ready: boolean;
    terminal: boolean;
  }

  let terminalEl: HTMLElement;
  let term: Terminal;
  let fitAddon: FitAddon;
  let ws: WebSocket | null = null;

  let namespaces: string[] = [];
  let namespace = 'naslos';
  let pods: Pod[] = [];
  // The privileged shell container the chart deploys (FR-LOG-02) is the default
  // target: it is the only pod that exists to be a terminal.
  let terminalPod: Pod | null = null;
  let pod = '';
  let container = '';
  let shell = 'bash';

  let connecting = false;
  let connected = false;
  let error = '';
  let loadingTargets = true;

  function selectedPod(): Pod | undefined {
    return pods.find(p => p.name === pod);
  }

  function containersFor(name: string): string[] {
    return pods.find(p => p.name === name)?.containers ?? [];
  }

  async function loadNamespaces() {
    try {
      const res = await fetch('/api/namespaces');
      if (!res.ok) return;
      const data = await res.json();
      namespaces = Array.isArray(data) ? data : [];
      // Prefer the namespace Naslos runs in when it is among them.
      if (namespaces.includes('naslos')) namespace = 'naslos';
      else if (namespaces.length > 0 && !namespaces.includes(namespace)) namespace = namespaces[0];
    } catch (e) {
      // The picker keeps its default; a real failure surfaces on connect.
    }
  }

  async function loadPods() {
    loadingTargets = true;
    error = '';
    try {
      const res = await fetch(`/api/pods?namespace=${encodeURIComponent(namespace)}`);
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || `Could not list pods (HTTP ${res.status})`);
      pods = Array.isArray(data) ? data : [];

      terminalPod = pods.find(p => p.terminal) ?? null;

      // Keep the selection valid: the current pod if it is still there,
      // otherwise the shell container, otherwise the first pod.
      if (!pods.some(p => p.name === pod)) {
        pod = terminalPod?.name ?? pods[0]?.name ?? '';
      }
      if (!containersFor(pod).includes(container)) {
        container = containersFor(pod)[0] ?? '';
      }
    } catch (e: any) {
      error = e.message;
      pods = [];
    } finally {
      loadingTargets = false;
    }
  }

  function onNamespaceChange() {
    pod = '';
    container = '';
    loadPods();
  }

  function onPodChange() {
    container = containersFor(pod)[0] ?? '';
  }

  function writeBanner() {
    term.writeln('\x1b[36m╔══════════════════════════════════════╗\x1b[0m');
    term.writeln('\x1b[36m║\x1b[0m  \x1b[1mNaslos Web Terminal\x1b[0m                  \x1b[36m║\x1b[0m');
    term.writeln('\x1b[36m╚══════════════════════════════════════╝\x1b[0m');
    term.writeln('');
  }

  // sendSize tells the server how big the terminal is. Without it the remote TTY
  // stays 80x24 and full-screen tools draw into the wrong shape.
  function sendSize() {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows }));
    }
  }


  async function connect() {
    error = '';
    if (!pod) {
      error = 'Choose a pod to connect to.';
      return;
    }

    const params = new URLSearchParams({ namespace, pod, container, shell });
    const apiUrl = `/api/ws/exec?${params.toString()}`;

    connecting = true;
    try {
      // Preflight as a plain GET: a browser cannot read the status of a failed
      // websocket handshake, so this is what turns "pod not found" or "several
      // containers, pick one" into a message the operator can act on.
      const check = await fetch(apiUrl);
      if (!check.ok) {
        const data = await check.json().catch(() => ({}));
        throw new Error(data.error || `Cannot attach (HTTP ${check.status})`);
      }
      const resolved = await check.json();
      container = resolved.container ?? container;
    } catch (e: any) {
      error = e.message;
      connecting = false;
      return;
    }

    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    term.writeln(`\r\n\x1b[36mAttaching to ${namespace}/${pod}${container ? ` (${container})` : ''}…\x1b[0m`);

    ws = new WebSocket(`${proto}//${window.location.host}${apiUrl}`);

    ws.onopen = () => {
      connected = true;
      connecting = false;
      sendSize();
      term.focus();
    };

    ws.onmessage = (event) => {
      term.write(typeof event.data === 'string' ? event.data : new Uint8Array(event.data));
    };

    ws.onerror = () => {
      error = 'The terminal connection failed.';
      connected = false;
      connecting = false;
    };

    ws.onclose = () => {
      connected = false;
      connecting = false;
      term.writeln('\r\n\x1b[33mSession closed.\x1b[0m');
    };
  }

  function disconnect() {
    if (ws) {
      ws.close();
      ws = null;
    }
    connected = false;
    connecting = false;
  }

  onMount(() => {
    term = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      theme: {
        background: '#0f172a',
        foreground: '#e2e8f0',
        cursor: '#3b82f6'
      },
      scrollback: 10000
    });

    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(terminalEl);
    fitAddon.fit();
    writeBanner();

    // Keystrokes go out as BINARY frames: text frames are reserved for control
    // messages (resize), so the two can never be confused.
    term.onData((data) => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(new TextEncoder().encode(data));
      }
    });

    const resizeObserver = new ResizeObserver(() => {
      fitAddon.fit();
      sendSize();
    });
    resizeObserver.observe(terminalEl);
    term.onResize(sendSize);

    loadNamespaces().then(loadPods);

    return () => {
      disconnect();
      term.dispose();
      resizeObserver.disconnect();
    };
  });

  onDestroy(disconnect);
</script>

<div class="max-w-6xl mx-auto">
  <div class="flex items-start justify-between gap-4 mb-4">
    <div>
      <h1 class="text-3xl font-bold mb-2">Terminal</h1>
      <p class="text-gray-400">
        A shell inside a container on the node. Talos itself has no shell, so node work
        happens from here: <code class="text-xs">zpool status</code> works directly, and
        <code class="text-xs">chroot /host …</code> runs the host's own tools.
      </p>
    </div>
    <div>
      {#if connected}
        <button class="btn btn-danger" on:click={disconnect}>Disconnect</button>
      {:else}
        <button class="btn btn-primary" on:click={connect} disabled={connecting || !pod}>
          {connecting ? 'Connecting...' : 'Connect'}
        </button>
      {/if}
    </div>
  </div>

  {#if error}
    <div class="card mb-4 border-red-700 bg-red-900/20 text-red-300 text-sm">{error}</div>
  {/if}

  {#if !loadingTargets && !terminalPod}
    <div class="card mb-4 text-sm text-gray-400">
      No terminal container is deployed in this namespace, so there is nothing built to
      attach to. Enable <code>terminal.enabled</code> in the chart for the privileged shell
      container, or attach to one of the pods below.
    </div>
  {/if}

  <!-- Target selection -->
  <div class="card mb-4">
    <div class="grid grid-cols-1 md:grid-cols-4 gap-3">
      <div>
        <label class="label" for="term-field-1">Namespace</label>
        <select id="term-field-1" class="input w-full" bind:value={namespace} on:change={onNamespaceChange}>
          {#each namespaces as ns}
            <option value={ns}>{ns}</option>
          {/each}
        </select>
      </div>
      <div class="md:col-span-2">
        <label class="label" for="term-field-2">Pod *</label>
        <select id="term-field-2" class="input w-full" bind:value={pod} on:change={onPodChange} disabled={loadingTargets}>
          {#if pods.length === 0}
            <option value="">{loadingTargets ? 'Loading pods...' : 'No pods'}</option>
          {/if}
          {#each pods as p}
            <option value={p.name}>{p.name}{p.terminal ? ' — shell' : ''}{p.ready ? '' : ` (${p.phase})`}</option>
          {/each}
        </select>
      </div>
      <div>
        <label class="label" for="term-field-3">Container</label>
        <select id="term-field-3" class="input w-full" bind:value={container}>
          {#each containersFor(pod) as c}
            <option value={c}>{c}</option>
          {/each}
        </select>
      </div>
    </div>

    <div class="flex items-end gap-3 mt-3 flex-wrap">
      <div class="w-32">
        <label class="label" for="term-field-4">Shell</label>
        <select id="term-field-4" class="input w-full" bind:value={shell}>
          {#each SHELLS as s}
            <option value={s}>{s}</option>
          {/each}
        </select>
      </div>
      <button class="btn btn-secondary" on:click={loadPods} disabled={loadingTargets}>Refresh</button>
      <button class="btn btn-secondary" on:click={() => term?.clear()}>Clear</button>
      <p class="text-xs text-gray-500">
        {selectedPod()?.terminal
          ? 'The terminal container: root, privileged, with the host mounted.'
          : 'A service container: it may have only sh.'}
      </p>
    </div>
  </div>

  <!-- Terminal -->
  <div class="card p-0 overflow-hidden">
    <div bind:this={terminalEl} class="h-[60vh] bg-naslos-dark"></div>
  </div>
</div>

