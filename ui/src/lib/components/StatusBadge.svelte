<script lang="ts">
  // Status badge with one tone mapping shared by pools, disks, VMs and job
  // states. Tone classes are theme-aware (see .badge-* in app.css).
  // App/install states are deliberately NOT mapped here: InstalledAppRow and
  // AppDetailDrawer keep $lib/appStatus.ts as the app-only tone table.
  export let state = '';
  export let label = '';

  type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

  // Explicit tone override for surfaces that need different semantics for the
  // same word (a running VM is healthy, a running job is in progress).
  export let tone: Tone | '' = '';
  // Fallback tone for states toneFor does not know. Pool health passes 'bad'
  // so an unmapped ZFS state fails closed, the way the old inline colors did.
  export let unknownTone: Tone = 'neutral';

  function toneFor(value: string, fallback: Tone): Tone {
    const s = value.toLowerCase();
    if (
      s === 'online' ||
      s === 'running' ||
      s === 'deployed' ||
      s === 'succeeded' ||
      s === 'ok' ||
      s === 'ready' ||
      s === 'active' ||
      s === 'passed'
    ) {
      return 'ok';
    }
    if (
      s === 'degraded' ||
      s === 'pending' ||
      s === 'uninstalling' ||
      s === 'standby' ||
      s === 'unknown' ||
      s === 'warn' ||
      s === 'warning' ||
      s === 'never run'
    ) {
      return 'warn';
    }
    if (
      s === 'faulted' ||
      s === 'failed' ||
      s === 'suspended' ||
      s === 'removed' ||
      s === 'unavail' ||
      s === 'error' ||
      s === 'not-ready' ||
      s === 'disabled'
    ) {
      // 'offline' is intentionally NOT bad: an administratively offlined
      // vdev is neutral in the device tree; pool-level surfaces pass
      // unknownTone='bad' so an OFFLINE pool still reads as an alert.
      return 'bad';
    }
    if (s === 'loading' || s === 'provisioning') {
      return 'info';
    }
    return fallback;
  }

  $: text = label || state || 'unknown';
  $: resolvedTone = (tone || toneFor(state, unknownTone)) as Tone;
</script>

<span class="badge badge-{resolvedTone}">{text}</span>
