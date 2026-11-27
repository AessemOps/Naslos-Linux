<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  // The xterm packages were deprecated and renamed to the @xterm scope (CR-31).
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import '@xterm/xterm/css/xterm.css';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { theme } from '$lib/stores/theme';

  // Shell names mirror the API's allowlist (api/internal/server/websocket.go):
  // the terminal cannot run an arbitrary command, only a shell.
  const SHELLS = ['bash', 'sh', 'ash', 'zsh'];

  // xterm renders to a canvas, so it cannot inherit the CSS variables; keep the
  // two palettes in sync with the --n-* tokens in app.css.
  const XTERM_THEMES = {
    dark: {
      background: '#0f172a',
      foreground: '#e2e8f0',
      cursor: '#3b82f6',
      selectionBackground: '#334155'
    },
    light: {
      background: '#ffffff',
      foreground: '#0f172a',
      cursor: '#2563eb',
      selectionBackground: '#dbeafe'
    }
  } as const;

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
      // Prefer the namespace that actually hosts the terminal container. Since
      // the privileged workloads moved to their own namespace (AUDIT-M6), the
      // old hardcoded preference for "naslos" landed the picker on a namespace
      // with no terminal pod. Probe each candidate (the likely one first) and
      // keep the first that has a terminal pod; fall back to the release
      // namespace, then the first listed.
      const preferred = ['naslos-privileged', 'naslos'];
      const ordered = [
        ...preferred.filter(ns => namespaces.includes(ns)),
        ...namespaces.filter(ns => !preferred.includes(ns))
      ];
      for (const ns of ordered) {
        try {
          const pr = await fetch(`/api/pods?namespace=${encodeURIComponent(ns)}`);
          if (!pr.ok) continue;
          const pods = await pr.json();
          if (Array.isArray(pods) && pods.some(p => p.terminal)) {
            namespace = ns;
            return;
          }
        } catch {
          // Try the next candidate.
        }
      }
      // No terminal pod found anywhere: keep something valid so Connect can
      // explain the situation.
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
    if (connecting || (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING))) {
      return;
    }
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

    const socket = new WebSocket(`${proto}//${window.location.host}${apiUrl}`);
    ws = socket;
    const current = () => ws === socket;

    socket.onopen = () => {
      if (!current()) return;
      connected = true;
      connecting = false;
      sendSize();
      term.focus();
    };

    socket.onmessage = (event) => {
      if (!current()) return;
      term.write(typeof event.data === 'string' ? event.data : new Uint8Array(event.data));
    };

    socket.onerror = () => {
      if (!current()) return;
      error = 'The terminal connection failed.';
      connected = false;
      connecting = false;
    };

    socket.onclose = (event) => {
      if (!current()) return;
      connected = false;
      connecting = false;
      const reason = event.reason || 'Session closed.';
      term.writeln(`\r\n\x1b[33m${reason}\x1b[0m`);
      ws = null;
    };
  }

  function disconnect() {
    const socket = ws;
    ws = null;
    if (socket) {
      socket.close();
      // onclose deliberately ignores a socket that is no longer current, so
      // the user-initiated close must print the message here.
      if (term) term.writeln('\r\n\x1b[33mSession closed.\x1b[0m');
    }
    connected = false;
    connecting = false;
  }

  onMount(() => {
    term = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: '"Fira Code", monospace',
      theme: { ...XTERM_THEMES[get(theme)] },
      scrollback: 10000
    });

    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(terminalEl);
    fitAddon.fit();
    // Fira Code may finish loading after the first measure; refit so the
    // mobile console does not keep the fallback font's wider cell grid.
    document.fonts?.ready.then(() => {
      if (!term) return;
      fitAddon.fit();
      sendSize();
    });
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

    // Live theme flip: the canvas palette must follow the persisted theme.
    const unsubscribeTheme = theme.subscribe((value) => {
      if (term) term.options.theme = { ...XTERM_THEMES[value] };
    });

    return () => {
      unsubscribeTheme();
      disconnect();
      term.dispose();
      resizeObserver.disconnect();
    };
  });

  onDestroy(disconnect);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Terminal"
    subtitle="A shell inside a container on the node. Talos itself has no shell, so node work happens from here: zpool status works directly, and chroot /host … runs the host's own tools."
  >
    <div slot="actions">
      {#if connected}
        <Button variant="danger" on:click={disconnect}>Disconnect</Button>
      {:else}
        <Button on:click={connect} loading={connecting} disabled={!pod}>Connect</Button>
      {/if}
    </div>
  </PageHeader>

  {#if error}
    <div role="alert" class="callout callout-bad">
      <p>{error}</p>
      <Button variant="secondary" size="sm" on:click={loadPods}>Retry</Button>
    </div>
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
      <FormField label="Namespace" id="term-field-1">
        <select id="term-field-1" class="input w-full" bind:value={namespace} on:change={onNamespaceChange}>
          {#each namespaces as ns}
            <option value={ns}>{ns}</option>
          {/each}
        </select>
      </FormField>
      <FormField label="Pod" id="term-field-2" required>
        <select id="term-field-2" class="input w-full" bind:value={pod} on:change={onPodChange} disabled={loadingTargets}>
          {#if pods.length === 0}
            <option value="">{loadingTargets ? 'Loading pods...' : 'No pods'}</option>
          {/if}
          {#each pods as p}
            <option value={p.name}>{p.name}{p.terminal ? ' — shell' : ''}{p.ready ? '' : ` (${p.phase})`}</option>
          {/each}
        </select>
      </FormField>
      <FormField label="Container" id="term-field-3">
        <select id="term-field-3" class="input w-full" bind:value={container}>
          {#each containersFor(pod) as c}
            <option value={c}>{c}</option>
          {/each}
        </select>
      </FormField>
    </div>

    <div class="flex items-end gap-3 mt-3 flex-wrap">
      <div class="w-32">
        <FormField label="Shell" id="term-field-4">
          <select id="term-field-4" class="input w-full" bind:value={shell}>
            {#each SHELLS as s}
              <option value={s}>{s}</option>
            {/each}
          </select>
        </FormField>
      </div>
      <Button variant="secondary" loading={loadingTargets} on:click={loadPods}>Refresh</Button>
      <Button variant="secondary" on:click={() => term?.clear()}>Clear</Button>
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

