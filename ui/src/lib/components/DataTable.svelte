<script lang="ts">
  // Horizontal-scrolling frame for a real <table>. Styling lives in the
  // .data-table rules in app.css so callers need no table classes.
  //
  //   <DataTable>
  //     <table>…</table>
  //   </DataTable>
</script>

<div class="data-table">
  <slot />
</div>
