<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { Terminal } from 'xterm';
  import { FitAddon } from 'xterm-addon-fit';
  import 'xterm/css/xterm.css';

  let terminalEl: HTMLElement;
  let term: Terminal;
  let fitAddon: FitAddon;
  let ws: WebSocket | null = null;
  let connected = false;
  let namespace = 'naslos';
  let pod = '';
  let container = '';
  let command = '/bin/zsh';

  function connect() {
    if (!pod) {
      term.writeln('\r\n\x1b[33mPlease enter a pod name to connect.\x1b[0m');
      return;
    }

    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${window.location.host}/api/ws/exec?namespace=${namespace}&pod=${pod}&container=${container}&command=${command}`;

    term.writeln(`\r\n\x1b[36mConnecting to ${pod} (${container})...\x1b[0m\r\n`);

    ws = new WebSocket(url);
    connected = true;

    ws.onopen = () => {
      term.writeln('\x1b[32mConnected.\x1b[0m\r\n');
    };

    ws.onmessage = (event) => {
      term.write(event.data);
    };

    ws.onerror = () => {
      term.writeln('\r\n\x1b[31mWebSocket error occurred.\x1b[0m');
      connected = false;
    };

    ws.onclose = () => {
      term.writeln('\r\n\x1b[33mConnection closed.\x1b[0m');
      connected = false;
    };

    // Forward terminal input to WebSocket
    term.onData((data) => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(data);
      }
    });
  }

  function disconnect() {
    if (ws) {
      ws.close();
      ws = null;
    }
    connected = false;
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

    term.writeln('\x1b[36m╔══════════════════════════════════════╗\x1b[0m');
    term.writeln('\x1b[36m║\x1b[0m  \x1b[1mNaslos Web Terminal\x1b[0m                  \x1b[36m║\x1b[0m');
    term.writeln('\x1b[36m║\x1b[0m  Enter pod details above to connect   \x1b[36m║\x1b[0m');
    term.writeln('\x1b[36m╚══════════════════════════════════════╝\x1b[0m');
    term.writeln('');

    // Handle resize
    const resizeObserver = new ResizeObserver(() => fitAddon.fit());
    resizeObserver.observe(terminalEl);

    return () => {
      disconnect();
      term.dispose();
      resizeObserver.disconnect();
    };
  });

  onDestroy(() => {
    disconnect();
  });
</script>

<div class="max-w-6xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Terminal</h1>
  <p class="text-gray-400 mb-4">Connect to a pod via WebSocket for interactive shell access.</p>

  <!-- Connection bar -->
  <div class="card mb-4">
    <div class="flex gap-3 items-end">
      <div class="flex-1">
        <label class="label">Namespace</label>
        <input type="text" bind:value={namespace} class="input w-full" />
      </div>
      <div class="flex-1">
        <label class="label">Pod Name *</label>
        <input type="text" bind:value={pod} placeholder="e.g. naslos-api-abc123" class="input w-full" />
      </div>
      <div class="flex-1">
        <label class="label">Container</label>
        <input type="text" bind:value={container} placeholder="default: main" class="input w-full" />
      </div>
      <div class="flex-1">
        <label class="label">Command</label>
        <select bind:value={command} class="input w-full">
          <option value="/bin/zsh">/bin/zsh</option>
          <option value="/bin/bash">/bin/bash</option>
          <option value="/bin/sh">/bin/sh</option>
        </select>
      </div>
      {#if connected}
        <button class="btn btn-danger" on:click={disconnect}>Disconnect</button>
      {:else}
        <button class="btn btn-primary" on:click={connect}>Connect</button>
      {/if}
    </div>
  </div>

  <!-- Terminal -->
  <div class="card p-0 overflow-hidden">
    <div bind:this={terminalEl} class="h-[500px] bg-naslos-dark"></div>
  </div>
</div>
