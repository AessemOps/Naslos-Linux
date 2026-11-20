<script lang="ts">
  // Status badge with one tone mapping shared by pools, disks, VMs, jobs and
  // app rows. Tone classes are theme-aware (see .badge-* in app.css).
  export let state = '';
  export let label = '';

  type Tone = 'ok' | 'warn' | 'bad' | 'info' | 'neutral';

  function toneFor(value: string): Tone {
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
      s === 'offline' ||
      s === 'unavail' ||
      s === 'error' ||
      s === 'not-ready' ||
      s === 'disabled'
    ) {
      return 'bad';
    }
    if (s === 'loading' || s === 'provisioning') {
      return 'info';
    }
    return 'neutral';
  }

  $: text = label || state || 'unknown';
  $: tone = toneFor(state);
</script>

<span class="badge badge-{tone}">{text}</span>
