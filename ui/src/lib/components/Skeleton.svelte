<script lang="ts">
  // Reduced-motion-safe placeholder block. The global
  // prefers-reduced-motion rule collapses the pulse to a static tint.
  export let width = 'w-full';
  export let height = 'h-4';
  export let rounded = 'rounded-md';
  export let label = '';
</script>

<div
  class="skeleton {width} {height} {rounded}"
  role={label ? 'status' : undefined}
  aria-label={label || undefined}
  aria-hidden={label ? undefined : 'true'}
></div>
