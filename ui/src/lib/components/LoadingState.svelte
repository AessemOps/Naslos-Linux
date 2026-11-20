<script lang="ts">
  // Shared loading state: one markup/aria pattern for every page. The label
  // text is per-page on purpose — the Playwright suite asserts the exact
  // loader strings (e.g. 'Loading dashboard...'), so callers pass their
  // existing copy and the specs stay green.
  export let label = 'Loading…';
</script>

<div class="py-12" role="status" aria-busy="true" data-testid="page-loading">
  <div class="flex items-center gap-3 text-gray-400">
    <span
      class="w-5 h-5 rounded-full border-2 border-naslos-border border-t-naslos-primary animate-spin shrink-0"
      aria-hidden="true"
    ></span>
    <span>{label}</span>
  </div>
</div>
