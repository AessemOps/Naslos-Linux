<script lang="ts">
  import { onMount } from 'svelte';
  import { Terminal } from 'xterm';
  import { FitAddon } from 'xterm-addon-fit';
  import 'xterm/css/xterm.css';

  let terminalEl: HTMLElement;
  let term: Terminal;

  onMount(() => {
    term = new Terminal({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      theme: {
        background: '#0f172a',
        foreground: '#e2e8f0'
      }
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(terminalEl);
    fitAddon.fit();

    term.writeln('Welcome to NasOS Terminal');
    term.writeln('Connecting to zsh shell...');
    term.writeln('');

    // In production: connect to WebSocket terminal session
    // For now, show a placeholder
    term.write('$ ');

    return () => term.dispose();
  });
</script>

<div class="max-w-6xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Terminal</h1>
  <p class="text-gray-400 mb-4">Zsh shell access to your NAS.</p>

  <div class="card p-0 overflow-hidden">
    <div bind:this={terminalEl} class="h-[600px]"></div>
  </div>
</div>
