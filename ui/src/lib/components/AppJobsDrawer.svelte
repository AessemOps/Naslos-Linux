<script lang="ts">
  import { appJobs, dismissAppJob, type AppJobKind } from '$lib/stores/appJobs';
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
  <div class="fixed bottom-4 right-4 z-30 w-[min(20rem,calc(100vw-2rem))]">
    <div class="overflow-hidden rounded-xl border border-naslos-border bg-naslos-surface shadow-lg">
      <button
        class="flex min-h-10 w-full cursor-pointer items-center justify-between px-3 py-2 text-sm"
        aria-expanded={open}
        on:click={() => (open = !open)}
      >
        <span class="font-medium">App jobs{running > 0 ? ` (${running} running)` : ''}</span>
        <span class="text-gray-400" aria-hidden="true"><Icon name={open ? 'chevron-down' : 'chevron-right'} size={16} /></span>
      </button>
      {#if open}
        <div class="max-h-64 divide-y divide-naslos-border overflow-y-auto">
          {#each jobs as job (job.id)}
            <div class="px-3 py-2 text-sm">
              <div class="flex items-center justify-between gap-2">
                <span class="truncate font-medium">{job.app}</span>
                <!-- A running job is in progress, not successful: override the
                     'running'→ok mapping (kept for VMs) back to warn. -->
                <StatusBadge state={job.state} tone={job.state === 'running' ? 'warn' : ''} />
              </div>
              <p class="mt-0.5 text-xs text-gray-400">
                {kindLabel(job.kind)}{job.state === 'running' ? ` · ${job.stage}` : ''}
              </p>
              {#if job.state === 'running'}
                <!-- The API does not expose a numeric percentage, so this is an
                     honest indeterminate bar: no aria-valuenow. -->
                <div
                  class="mt-2 h-1.5 w-full overflow-hidden rounded border border-naslos-border bg-naslos-dark"
                  role="progressbar"
                  aria-label={`${kindLabel(job.kind)} in progress`}
                >
                  <div class="job-progress h-1.5 w-1/3"></div>
                </div>
                {#if job.message}<p class="mt-1 text-xs text-gray-500">{job.message}</p>{/if}
              {:else if job.error}
                <p role="alert" class="mt-1 break-words text-xs text-red-300">{job.error}</p>
              {/if}
              <div class="mt-1 flex items-center justify-between">
                {#if job.state === 'succeeded' && job.kind === 'install'}
                  <a class="text-xs text-naslos-link hover:underline" href="/apps">View installed</a>
                {:else}
                  <span></span>
                {/if}
                <button
                  class="py-1.5 text-xs text-gray-500 hover:text-naslos-strong disabled:cursor-not-allowed disabled:opacity-40"
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
