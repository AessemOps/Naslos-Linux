<script lang="ts">
  import { appToasts, dismissToast } from '$lib/stores/appJobs';
  import Icon from './Icon.svelte';
</script>

<div
  class="fixed right-4 top-4 z-[60] flex w-[min(20rem,calc(100vw-2rem))] flex-col gap-2"
  aria-live="polite"
  data-modal-exempt
>
  {#each $appToasts as toast (toast.id)}
    <div
      class="card border p-3 text-sm {toast.tone === 'success'
        ? 'border-green-700 bg-green-900/40 text-green-200'
        : 'border-red-700 bg-red-900/40 text-red-200'}"
      role={toast.tone === 'success' ? 'status' : 'alert'}
    >
      <div class="flex items-start justify-between gap-2">
        <div class="flex min-w-0 items-start gap-2">
          <Icon name={toast.tone === 'success' ? 'circle-check' : 'triangle-alert'} size={18} />
          <div class="min-w-0">
            <p class="font-medium">{toast.title}</p>
            {#if toast.detail}<p class="mt-1 break-words text-xs">{toast.detail}</p>{/if}
          </div>
        </div>
        <button
          class="icon-btn -m-1 text-gray-300 hover:text-naslos-strong"
          aria-label="Dismiss"
          on:click={() => dismissToast(toast.id)}
        ><Icon name="x" size={16} /></button>
      </div>
    </div>
  {/each}
</div>
