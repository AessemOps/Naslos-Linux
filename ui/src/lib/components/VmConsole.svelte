<script lang="ts">
  import { onDestroy } from 'svelte';
  // The official noVNC distribution (MPL-2.0, see CREDITS.md): it speaks RFB
  // over the websocket the API bridges to QEMU's password-protected VNC.
  // It touches `window` at module scope, so it is imported lazily when the
  // console is opened instead of on the SSR/preview path.
  import Button from './Button.svelte';

  export let vm: {
    name: string;
    state: string;
  };

  let consoleEl: HTMLElement;
  let rfb: any = null;
  let connected = false;
  let connecting = false;
  let error = '';
  let destroyed = false;
  let preflight: AbortController | null = null;
  let connectSeq = 0;

  async function connect() {
    error = '';
    if (connecting || connected) return;
    connecting = true;
    const seq = ++connectSeq;
    preflight?.abort();
    const controller = new AbortController();
    preflight = controller;
    try {
      // Plain-GET preflight: surfaces "not running"/"no KVM" as a readable
      // message (a browser cannot read a failed websocket handshake) and
      // hands over the per-boot VNC password for the RFB auth handshake.
      const res = await fetch(`/api/ws/vm/${encodeURIComponent(vm.name)}/vnc`, {
        signal: controller.signal
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Cannot open the console (HTTP ${res.status})`);
      }
      const info = await res.json();
      // The drawer can close while the preflight is in flight; never attach
      // noVNC to a detached element or after a disconnect.
      if (destroyed || seq !== connectSeq) return;
      const { default: RFB } = await import('@novnc/novnc');
      if (destroyed || seq !== connectSeq) return;
      const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const url = `${proto}//${window.location.host}/api/ws/vm/${encodeURIComponent(vm.name)}/vnc`;
      const instance = new RFB(consoleEl, url, {
        credentials: { password: info.vncPassword },
        shared: false
      });
      rfb = instance;
      instance.scaleViewport = true;
      instance.resizeSession = false;
      instance.addEventListener('connect', () => {
        if (destroyed || instance !== rfb) {
          instance.disconnect();
          return;
        }
        connected = true;
        connecting = false;
      });
      instance.addEventListener('disconnect', (event: any) => {
        if (destroyed) return;
        connected = false;
        connecting = false;
        if (!event.detail.clean) {
          error = 'The console connection closed.';
        }
      });
    } catch (e: any) {
      if (destroyed || controller.signal.aborted) return;
      error = e.message;
    } finally {
      if (seq === connectSeq) connecting = false;
      if (preflight === controller) preflight = null;
    }
  }

  function disconnect() {
    connectSeq += 1;
    preflight?.abort();
    preflight = null;
    const instance = rfb;
    rfb = null;
    if (instance) instance.disconnect();
    connected = false;
    connecting = false;
  }

  function sendKeys() {
    if (rfb && connected) rfb.sendCtrlAltDel();
  }

  function fullscreen() {
    consoleEl?.requestFullscreen?.();
  }

  onDestroy(() => {
    destroyed = true;
    disconnect();
  });
</script>

<div class="flex items-center justify-between gap-2 mb-2 flex-wrap">
  <div class="flex gap-2">
    {#if connected}
      <Button variant="danger" on:click={disconnect}>Disconnect</Button>
      <Button variant="secondary" on:click={sendKeys} title="Send Ctrl+Alt+Del to the guest">Ctrl+Alt+Del</Button>
    {:else}
      <Button on:click={connect} loading={connecting} disabled={vm.state !== 'running'}>
        {connecting ? 'Connecting…' : 'Connect console'}
      </Button>
    {/if}
    <Button variant="secondary" on:click={fullscreen} disabled={!connected}>Fullscreen</Button>
  </div>
  <p class="text-xs text-gray-500">
    The console reaches the VM's VNC through the API; it needs the VM running.
  </p>
</div>

{#if error}
  <div role="alert" class="callout callout-bad">{error}</div>
{/if}

<div class="card p-0 overflow-hidden">
  <div bind:this={consoleEl} class="h-[60vh] bg-black"></div>
</div>
