<script lang="ts">
  import { onDestroy } from 'svelte';
  // The official noVNC distribution (MPL-2.0, see CREDITS.md): it speaks RFB
  // over the websocket the API bridges to QEMU's password-protected VNC.
  import RFB from '@novnc/novnc';

  export let vm: {
    name: string;
    state: string;
  };

  let consoleEl: HTMLElement;
  let rfb: any = null;
  let connected = false;
  let connecting = false;
  let error = '';

  async function connect() {
    error = '';
    if (connecting || connected) return;
    connecting = true;
    try {
      // Plain-GET preflight: surfaces "not running"/"no KVM" as a readable
      // message (a browser cannot read a failed websocket handshake) and
      // hands over the per-boot VNC password for the RFB auth handshake.
      const res = await fetch(`/api/ws/vm/${encodeURIComponent(vm.name)}/vnc`);
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Cannot open the console (HTTP ${res.status})`);
      }
      const info = await res.json();
      const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const url = `${proto}//${window.location.host}/api/ws/vm/${encodeURIComponent(vm.name)}/vnc`;
      rfb = new RFB(consoleEl, url, {
        credentials: { password: info.vncPassword },
        shared: false
      });
      rfb.scaleViewport = true;
      rfb.resizeSession = false;
      rfb.addEventListener('connect', () => {
        connected = true;
        connecting = false;
      });
      rfb.addEventListener('disconnect', (event: any) => {
        connected = false;
        connecting = false;
        if (!event.detail.clean) {
          error = 'The console connection closed.';
        }
      });
    } catch (e: any) {
      error = e.message;
      connecting = false;
    }
  }

  function disconnect() {
    if (rfb) {
      rfb.disconnect();
      rfb = null;
    }
    connected = false;
    connecting = false;
  }

  function sendKeys() {
    if (rfb && connected) rfb.sendCtrlAltDel();
  }

  function fullscreen() {
    consoleEl?.requestFullscreen?.();
  }

  onDestroy(disconnect);
</script>

<div class="flex items-center justify-between gap-2 mb-2 flex-wrap">
  <div class="flex gap-2">
    {#if connected}
      <button class="btn btn-danger" on:click={disconnect}>Disconnect</button>
      <button class="btn btn-secondary" on:click={sendKeys} title="Send Ctrl+Alt+Del to the guest">Ctrl+Alt+Del</button>
    {:else}
      <button class="btn btn-primary" on:click={connect} disabled={connecting || vm.state !== 'running'}>
        {connecting ? 'Connecting…' : 'Connect console'}
      </button>
    {/if}
    <button class="btn btn-secondary" on:click={fullscreen} disabled={!connected}>Fullscreen</button>
  </div>
  <p class="text-xs text-gray-500">
    The console reaches the VM's VNC through the API; it needs the VM running.
  </p>
</div>

{#if error}
  <div role="alert" class="card mb-2 border-red-700 bg-red-900/20 text-red-300 text-sm">{error}</div>
{/if}

<div class="card p-0 overflow-hidden">
  <div bind:this={consoleEl} class="h-[60vh] bg-black"></div>
</div>
