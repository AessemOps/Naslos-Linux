<script lang="ts">
  // One mutually exclusive decision point for a page's loading/error/empty
  // states. The labels are passed through unchanged so existing Playwright
  // assertions ("Loading users...", etc.) keep working.
  import EmptyState from './EmptyState.svelte';
  import ErrorState from './ErrorState.svelte';
  import LoadingState from './LoadingState.svelte';

  export let loading = false;
  export let error = '';
  export let empty = false;
  export let loadingText = 'Loading…';
  export let emptyTitle = 'Nothing here yet';
  export let emptyDetail = '';
  export let emptyIcon = 'inbox';
  export let onRetry: (() => void) | null = null;
</script>

{#if loading}
  <LoadingState label={loadingText} />
{:else if error}
  <ErrorState message={error} retry={onRetry} />
{:else if empty}
  <EmptyState icon={emptyIcon} title={emptyTitle} detail={emptyDetail} />
{:else}
  <slot />
{/if}
