<script lang="ts">
  import { appJobs, dismissAppJob, type AppJob, type AppJobKind } from '$lib/stores/appJobs';
  import Icon from './Icon.svelte';
  import StatusBadge from './StatusBadge.svelte';

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
</script>

{#if jobs.length > 0}
  <div class="fixed bottom-4 right-4 z-40 w-80">
    <div class="bg-naslos-surface border border-naslos-border rounded-xl shadow-lg overflow-hidden">
      <button
        class="w-full flex items-center justify-between px-3 py-2 text-sm cursor-pointer"
        aria-expanded={open}
        on:click={() => (open = !open)}
      >
        <span class="font-medium">App jobs{running > 0 ? ` (${running} running)` : ''}</span>
        <span class="text-gray-400" aria-hidden="true"><Icon name={open ? 'chevron-down' : 'chevron-right'} size={16} /></span>
      </button>
      {#if open}
        <div class="max-h-64 overflow-y-auto divide-y divide-naslos-border">
          {#each jobs as job (job.id)}
            <div class="px-3 py-2 text-sm">
              <div class="flex items-center justify-between gap-2">
                <span class="font-medium truncate">{job.app}</span>
                <!-- A running job is in progress, not successful: override the
                     'running'→ok mapping (kept for VMs) back to warn. -->
                <StatusBadge state={job.state} tone={job.state === 'running' ? 'warn' : ''} />
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
                <p role="alert" class="text-xs text-red-300 mt-1 break-words">{job.error}</p>
              {/if}
              <div class="flex items-center justify-between mt-1">
                {#if job.state === 'succeeded' && job.kind === 'install'}
                  <a class="text-xs text-naslos-accent hover:underline" href="/apps">View installed</a>
                {:else}
                  <span></span>
                {/if}
                <button
                  class="text-xs py-1.5 text-gray-500 hover:text-naslos-strong disabled:opacity-40 disabled:cursor-not-allowed"
                  disabled={job.state === 'running'}
                  title={job.state === 'running' ? 'Wait for the job to finish' : 'Dismiss'}
                  on:click={() => dismissAppJob(job.id)}
                >Dismiss</button>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}
