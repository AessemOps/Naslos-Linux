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
  // Slot `emptyAction`: buttons/links rendered inside the empty state (for
  // example "Create …"), so callers do not hand-roll EmptyState in the slot.
  //
  // Background polls should not pass their error here once data has loaded:
  // use `error={hasData ? '' : error}` and show a callout above the content
  // instead, so one failed refresh does not hide the last good snapshot.
  export let onRetry: (() => void) | null = null;
</script>

{#if loading}
  <LoadingState label={loadingText} />
{:else if error}
  <ErrorState message={error} retry={onRetry} />
{:else if empty}
  {#if $$slots.emptyAction}
    <EmptyState icon={emptyIcon} title={emptyTitle} detail={emptyDetail}>
      <svelte:fragment slot="action"><slot name="emptyAction" /></svelte:fragment>
    </EmptyState>
  {:else}
    <EmptyState icon={emptyIcon} title={emptyTitle} detail={emptyDetail} />
  {/if}
{:else}
  <slot />
{/if}
