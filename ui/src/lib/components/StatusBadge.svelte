<script lang="ts">
  // Status badge with one tone mapping shared by pools, disks, VMs and job
  // states. The word -> tone table lives in $lib/status.ts so non-badge
  // surfaces use exactly the same mapping. Tone classes are theme-aware (see
  // .badge-* in app.css).
  //
  // App/install states are deliberately NOT mapped here: InstalledAppRow and
  // AppDetailDrawer keep $lib/appStatus.ts as the app-only tone table.
  import { toneFor, type Tone } from '$lib/status';

  export let state = '';
  export let label = '';

  // Explicit tone override for surfaces that need different semantics for the
  // same word (a running VM is healthy, a running job is in progress).
  export let tone: Tone | '' = '';
  // Fallback tone for states toneFor does not know. Pool health passes 'bad'
  // so an unmapped ZFS state fails closed, the way the old inline colors did.
  export let unknownTone: Tone = 'neutral';

  $: text = label || state || 'unknown';
  $: resolvedTone = (tone || toneFor(state, unknownTone)) as Tone;
</script>

<span class="badge badge-{resolvedTone}">{text}</span>
