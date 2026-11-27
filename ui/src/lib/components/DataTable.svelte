<script lang="ts">
  // Horizontal-scrolling frame for a real <table>. Styling lives in the
  // .data-table rules in app.css so callers need no table classes.
  //
  //   <DataTable label="Users">
  //     <table>…</table>
  //   </DataTable>
  //
  // The frame is a focusable, labelled region so keyboard users can scroll a
  // wide table that has no focusable cells (axe scrollable-region-focusable).
  export let label = 'Table';
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="data-table" role="region" aria-label={label} tabindex="0">
  <slot />
</div>
