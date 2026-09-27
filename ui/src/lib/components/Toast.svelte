<script lang="ts">
  import { appToasts, dismissToast } from '$lib/stores/appJobs';
</script>

<div class="fixed top-4 right-4 z-[60] flex flex-col gap-2 w-80" aria-live="polite">
  {#each $appToasts as toast (toast.id)}
    <div
      class={`card border p-3 text-sm ${toast.tone === 'success' ? 'border-green-700 bg-green-900/40 text-green-200' : 'border-red-700 bg-red-900/40 text-red-200'}`}
      role="status"
    >
      <div class="flex items-start justify-between gap-2">
        <div class="min-w-0">
          <p class="font-medium">{toast.title}</p>
          {#if toast.detail}<p class="text-xs mt-1 break-words">{toast.detail}</p>{/if}
        </div>
        <button class="text-gray-300 hover:text-white" aria-label="Dismiss" on:click={() => dismissToast(toast.id)}>×</button>
      </div>
    </div>
  {/each}
</div>
