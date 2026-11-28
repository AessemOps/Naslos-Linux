<script lang="ts">
  // Cloud backup page: destinations, one-off sends, schedules, stored chains,
  // verify and restore. Load/state/poll glue only; each section lives in
  // $lib/components/cloud. The API contract is the frozen WP-1 surface
  // (/api/cloud/*, docs/api.md); the backend lands separately.
  import { onDestroy, onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import CloudChainsCard from '$lib/components/cloud/CloudChainsCard.svelte';
  import CloudDestinationModal from '$lib/components/cloud/CloudDestinationModal.svelte';
  import CloudDestinationsCard from '$lib/components/cloud/CloudDestinationsCard.svelte';
  import CloudRecoveryAck from '$lib/components/cloud/CloudRecoveryAck.svelte';
  import CloudRestoreModal from '$lib/components/cloud/CloudRestoreModal.svelte';
  import CloudSchedulesCard from '$lib/components/cloud/CloudSchedulesCard.svelte';
  import CloudSendCard from '$lib/components/cloud/CloudSendCard.svelte';
  import { createJobPoller } from '$lib/cloud/jobPoll';
  import { readDrAck, writeDrAck } from '$lib/cloud/drAck';
  import type {
    Chain,
    CloudJob,
    CloudSchedule,
    DatasetSummary,
    Destination,
    JobKind,
    StartedJob
  } from '$lib/cloud/types';

  interface RestoreTarget {
    destination: string;
    source: string;
    chain: Chain;
  }

  interface TrackedJob {
    id: string;
    kind: JobKind;
  }

  let loading = true;
  let error = '';
  let destinations: Destination[] = [];
  let schedules: CloudSchedule[] = [];
  let datasets: DatasetSummary[] = [];

  // The recovery-key panel stays until it is acknowledged in this browser.
  let drAck = false;

  let destinationModal = false;
  let editingDestination: Destination | null = null;
  let savedDestinationId = '';
  let destinationRevision = 0;
  let restoreTarget: RestoreTarget | null = null;

  let activeJob: CloudJob | null = null;
  let lastJob: TrackedJob | null = null;
  let pollFailed = false;
  let sendError = '';
  let jobError = '';
  let starting = false;
  let cancelling = false;

  // A send streams one dataset: the agent refuses the pool root itself, so
  // only child datasets are offered (same rule as the Buddy page).
  $: sendableDatasets = datasets.filter((d) => d.name.includes('/'));
  $: sendJob = activeJob?.kind === 'send' ? activeJob : null;
  $: chainJob = activeJob && activeJob.kind !== 'send' ? activeJob : null;
  $: restoreDestinationName = restoreTarget
    ? (destinations.find((d) => d.id === restoreTarget?.destination)?.name ??
      restoreTarget.destination)
    : '';

  function message(e: unknown): string {
    return e instanceof ApiError || e instanceof Error ? e.message : String(e);
  }

  const poller = createJobPoller({
    onUpdate: (job) => {
      activeJob = job;
      pollFailed = false;
    },
    onDone: (job) => {
      activeJob = job;
      pollFailed = false;
      lastJob = null;
      void afterJob(job);
    },
    // A send's failure belongs on the send card, a verify/restore failure on
    // the chains card. The kind comes from trackJob: activeJob is cleared
    // before the first poll, so it cannot identify a failed first poll.
    onError: (msg) => {
      activeJob = null;
      pollFailed = true;
      if (lastJob?.kind === 'send') sendError = msg;
      else jobError = msg;
    }
  });

  onDestroy(poller.stop);

  async function afterJob(job: CloudJob) {
    // A restore creates a dataset; a send moves the schedule's last run.
    if (job.kind === 'restore') await refreshDatasets();
    if (job.kind === 'send') await refreshSchedules();
  }

  async function loadAll() {
    loading = true;
    error = '';
    try {
      const [dest, schedulesData, datasetData] = await Promise.all([
        api<{ destinations?: Destination[] }>('/api/cloud/destinations'),
        api<{ schedules?: CloudSchedule[] }>('/api/cloud/schedules'),
        api<DatasetSummary[]>('/api/datasets')
      ]);
      destinations = Array.isArray(dest.destinations) ? dest.destinations : [];
      schedules = Array.isArray(schedulesData.schedules) ? schedulesData.schedules : [];
      datasets = Array.isArray(datasetData) ? datasetData : [];
      await resumeRunningJob();
    } catch (e) {
      error = 'Failed to load cloud backup: ' + message(e);
    } finally {
      loading = false;
    }
  }

  // Reattach to a job left running by an earlier visit. A failure here must
  // not hide the data that already loaded.
  async function resumeRunningJob() {
    try {
      const data = await api<{ jobs?: CloudJob[] }>('/api/cloud/jobs');
      const running = (Array.isArray(data.jobs) ? data.jobs : []).find(
        (j) => j.state === 'running'
      );
      if (running && !poller.jobId) {
        lastJob = { id: running.id, kind: running.kind };
        activeJob = running;
        pollFailed = false;
        poller.start(running.id);
      }
    } catch {
      // The job list is a convenience; the page works without it.
    }
  }

  async function refreshDestinations() {
    try {
      const data = await api<{ destinations?: Destination[] }>('/api/cloud/destinations');
      destinations = Array.isArray(data.destinations) ? data.destinations : [];
    } catch (e) {
      console.error('destinations reload failed', e);
    }
  }

  async function refreshSchedules() {
    try {
      const data = await api<{ schedules?: CloudSchedule[] }>('/api/cloud/schedules');
      schedules = Array.isArray(data.schedules) ? data.schedules : [];
    } catch (e) {
      console.error('schedules reload failed', e);
    }
  }

  async function refreshDatasets() {
    try {
      const data = await api<DatasetSummary[]>('/api/datasets');
      datasets = Array.isArray(data) ? data : [];
    } catch (e) {
      console.error('datasets reload failed', e);
    }
  }

  function trackJob(jobId: string, kind: JobKind) {
    lastJob = { id: jobId, kind };
    pollFailed = false;
    jobError = '';
    sendError = '';
    cancelling = false;
    activeJob = null;
    poller.start(jobId);
  }

  // A poll failure stops the poller and clears activeJob so the forms are
  // usable again; the server job may still be running, so let the user resume
  // watching it.
  function retryStatus() {
    if (!lastJob) return;
    pollFailed = false;
    jobError = '';
    sendError = '';
    cancelling = false;
    activeJob = null;
    poller.start(lastJob.id);
  }

  function acknowledgeRecovery() {
    writeDrAck();
    drAck = true;
  }

  function addDestination() {
    editingDestination = null;
    destinationModal = true;
  }

  function editDestination(event: CustomEvent<Destination>) {
    editingDestination = event.detail;
    destinationModal = true;
  }

  function closeDestinationModal() {
    destinationModal = false;
    editingDestination = null;
  }

  function destinationSaved(event: CustomEvent<Destination>) {
    closeDestinationModal();
    // A credential-only edit leaves the has* flags unchanged, so the card must
    // drop that destination's cached test result on every save.
    savedDestinationId = event.detail?.id ?? '';
    destinationRevision += 1;
    void refreshDestinations();
  }

  async function send(event: CustomEvent<{ dataset: string; destination: string; forceFull: boolean }>) {
    sendError = '';
    jobError = '';
    activeJob = null;
    lastJob = null;
    pollFailed = false;
    poller.stop();
    starting = true;
    try {
      const started = await api<StartedJob>('/api/cloud/send', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          dataset: event.detail.dataset,
          destination: event.detail.destination,
          forceFull: event.detail.forceFull
        })
      });
      trackJob(started.jobId, 'send');
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        sendError =
          'A backup of this dataset to this destination is already running. Wait for it to finish, or cancel it first.';
      } else if (e instanceof ApiError && e.status === 412) {
        sendError =
          'This instance has no backup identity yet. Create the Buddy identity on the Backups page first.';
      } else {
        sendError = 'Starting the send failed: ' + message(e);
      }
    } finally {
      starting = false;
    }
  }

  async function cancelJob() {
    if (!activeJob) return;
    cancelling = true;
    jobError = '';
    sendError = '';
    try {
      await poller.cancel();
    } finally {
      cancelling = false;
    }
  }

  function verifyStarted(event: CustomEvent<string>) {
    trackJob(event.detail, 'verify');
  }

  function restoreStarted(event: CustomEvent<string>) {
    restoreTarget = null;
    trackJob(event.detail, 'restore');
  }

  function openRestore(event: CustomEvent<RestoreTarget>) {
    restoreTarget = event.detail;
  }

  onMount(() => {
    drAck = readDrAck();
    void loadAll();
  });
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Cloud backup"
    subtitle="Push encrypted ZFS backups to S3-compatible storage. The bucket cannot read them without this NAS's keys."
    breadcrumbs={[
      { label: 'System' },
      { label: 'Backups', href: '/backups' },
      { label: 'Cloud backup' }
    ]}
  />

  <AsyncState
    {loading}
    error={error}
    loadingText="Loading cloud backup..."
    onRetry={loadAll}
  >
    <CloudRecoveryAck acknowledged={drAck} on:acknowledge={acknowledgeRecovery} />

    <CloudDestinationsCard
      {destinations}
      {savedDestinationId}
      savedRevision={destinationRevision}
      on:add={addDestination}
      on:edit={editDestination}
      on:refresh={refreshDestinations}
    />

    <CloudSendCard
      datasets={sendableDatasets}
      {destinations}
      job={sendJob}
      jobError={sendError}
      retry={pollFailed ? retryStatus : null}
      {starting}
      {cancelling}
      allowed={drAck}
      on:send={send}
      on:cancel={cancelJob}
    />

    <CloudSchedulesCard
      {schedules}
      datasets={sendableDatasets}
      {destinations}
      allowed={drAck}
      on:refresh={refreshSchedules}
    />

    <CloudChainsCard
      {destinations}
      datasets={sendableDatasets}
      job={chainJob}
      {jobError}
      retry={pollFailed ? retryStatus : null}
      {cancelling}
      on:started={verifyStarted}
      on:restore={openRestore}
      on:cancel={cancelJob}
    />
  </AsyncState>

  {#if destinationModal}
    <CloudDestinationModal
      destination={editingDestination}
      on:saved={destinationSaved}
      on:close={closeDestinationModal}
    />
  {/if}

  {#if restoreTarget}
    <CloudRestoreModal
      destination={restoreTarget.destination}
      destinationName={restoreDestinationName}
      source={restoreTarget.source}
      chain={restoreTarget.chain}
      {datasets}
      on:started={restoreStarted}
      on:close={() => (restoreTarget = null)}
    />
  {/if}
</div>
