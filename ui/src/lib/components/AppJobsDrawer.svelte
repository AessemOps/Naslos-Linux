<script lang="ts">
  import { appJobs, dismissAppJob, type AppJob, type AppJobKind, type AppJobState } from '$lib/stores/appJobs';

  let open = true;

  $: jobs = $appJobs;
  $: running = jobs.filter((j) => j.state === 'running').length;

  function kindLabel(kind: AppJobKind): string {
    switch (kind) {
      case 'install':
        return 'Install';
      case 'upgrade':
        return 'Update';
      default:
        return 'Uninstall';
    }
  }

  function stateClass(state: AppJobState): string {
    switch (state) {
      case 'succeeded':
        return 'bg-green-900/50 text-green-400';
      case 'failed':
        return 'bg-red-900/50 text-red-400';
      default:
        return 'bg-yellow-900/50 text-yellow-400';
    }
  }
</script>

{#if jobs.length > 0}
  <div class="fixed bottom-4 right-4 z-40 w-80">
    <div class="bg-naslos-surface border border-naslos-border rounded-xl shadow-lg overflow-hidden">
      <button class="w-full flex items-center justify-between px-3 py-2 text-sm" on:click={() => (open = !open)}>
        <span class="font-medium">App jobs{running > 0 ? ` (${running} running)` : ''}</span>
        <span class="text-gray-400">{open ? '▾' : '▸'}</span>
      </button>
      {#if open}
        <div class="max-h-64 overflow-y-auto divide-y divide-naslos-border">
          {#each jobs as job (job.id)}
            <div class="px-3 py-2 text-sm">
              <div class="flex items-center justify-between gap-2">
                <span class="font-medium truncate">{job.app}</span>
                <span class={`text-xs px-2 py-0.5 rounded ${stateClass(job.state)}`}>{job.state}</span>
              </div>
              <p class="text-xs text-gray-400 mt-0.5">
                {kindLabel(job.kind)}{job.state === 'running' ? ` · ${job.stage}` : ''}
              </p>
              {#if job.state === 'running'}
                <div class="w-full bg-naslos-dark border border-naslos-border rounded h-1.5 mt-2 overflow-hidden">
                  <div class="bg-naslos-primary h-1.5 animate-pulse" style="width: 60%"></div>
                </div>
                {#if job.message}<p class="text-xs text-gray-500 mt-1">{job.message}</p>{/if}
              {:else if job.error}
                <p class="text-xs text-red-300 mt-1 break-words">{job.error}</p>
              {/if}
              <div class="flex items-center justify-between mt-1">
                {#if job.state === 'succeeded' && job.kind === 'install'}
                  <a class="text-xs text-naslos-accent hover:underline" href="/apps">View installed</a>
                {:else}
                  <span></span>
                {/if}
                <button class="text-xs text-gray-500 hover:text-white" on:click={() => dismissAppJob(job.id)}>Dismiss</button>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}
