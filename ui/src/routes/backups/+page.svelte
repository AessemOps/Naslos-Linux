<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import BackupIdentityCard from '$lib/components/backups/BackupIdentityCard.svelte';
  import BackupReceiverCard from '$lib/components/backups/BackupReceiverCard.svelte';
  import BackupRestoreCard from '$lib/components/backups/BackupRestoreCard.svelte';
  import BackupSchedulesCard from '$lib/components/backups/BackupSchedulesCard.svelte';
  import BackupSendCard from '$lib/components/backups/BackupSendCard.svelte';
  import type {
    BackupJob,
    DatasetSummary,
    Identity,
    ReceiverStatus,
    Schedule
  } from '$lib/components/backups/types';
  import PageHeader from '$lib/components/PageHeader.svelte';

  let loading = true;
  let error = '';
  let identity: Identity | null = null;
  let creatingIdentity = false;
  let identityError = '';
  let copied = '';

  let schedules: Schedule[] = [];
  let datasets: DatasetSummary[] = [];
  // A send streams one dataset: the agent refuses the pool root itself
  // ("dataset must be <pool>/<name>"), so only child datasets are offered.
  $: sendableDatasets = datasets.filter((d) => d.name.includes('/'));

  let activeJob: BackupJob | null = null;
  let jobError = '';
  let starting = false;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let pollInFlight = false;

  let receiver: ReceiverStatus | null = null;
  let receiverMissing = false;
  let receiverError = '';

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function loadIdentity(): Promise<Identity | null> {
    try {
      return await api<Identity>('/api/buddy/identity');
    } catch (e) {
      // A 401 means the identity is not readable in this session; that is a
      // real page error, but a missing identity has its own exists=false shape.
      if (e instanceof ApiError && e.status === 401) return null;
      throw e;
    }
  }

  // The receiver answers 503 when Buddy Backup is not configured, which is a
  // setup state rather than an error. Any other non-2xx used to leave the card
  // blank; it now surfaces an error with a retry.
  async function loadReceiver() {
    receiverError = '';
    try {
      const res = await fetch('/api/buddy/status');
      if (res.ok) {
        receiver = await res.json();
        receiverMissing = false;
      } else if (res.status === 503) {
        receiver = null;
        receiverMissing = true;
      } else {
        receiver = null;
        receiverMissing = false;
        receiverError = `Receiver status unavailable (HTTP ${res.status})`;
      }
    } catch (e) {
      receiver = null;
      receiverMissing = false;
      receiverError = 'Failed to load the receiver status: ' + message(e);
    }
  }

  async function loadAll() {
    loading = true;
    error = '';
    try {
      const [id, scheduleData, datasetData] = await Promise.all([
        loadIdentity(),
        api<{ schedules?: Schedule[] }>('/api/buddy/schedules'),
        api<DatasetSummary[]>('/api/datasets')
      ]);
      identity = id;
      schedules = Array.isArray(scheduleData.schedules) ? scheduleData.schedules : [];
      datasets = Array.isArray(datasetData) ? datasetData : [];
    } catch (e) {
      error = 'Failed to load backup state: ' + message(e);
    } finally {
      loading = false;
    }
    await loadReceiver();
  }

  async function loadSchedules() {
    try {
      const data = await api<{ schedules?: Schedule[] }>('/api/buddy/schedules');
      schedules = Array.isArray(data.schedules) ? data.schedules : [];
    } catch (e) {
      console.error('schedules reload failed', e);
    }
  }

  async function createIdentity(event: CustomEvent<{ name: string }>) {
    creatingIdentity = true;
    identityError = '';
    try {
      identity = await api<Identity>('/api/buddy/identity', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: event.detail.name })
      });
    } catch (e) {
      identityError = 'Creating the identity failed: ' + message(e);
    } finally {
      creatingIdentity = false;
    }
  }

  async function copyKey() {
    if (!identity?.publicKey) return;
    try {
      await navigator.clipboard.writeText(identity.publicKey);
      copied = 'key';
      setTimeout(() => {
        if (copied === 'key') copied = '';
      }, 2000);
    } catch (e) {
      console.error('copy failed', e);
    }
  }

  function stopPoll() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  async function pollJob(jobId: string) {
    if (pollInFlight) return;
    pollInFlight = true;
    try {
      const res = await fetch(`/api/buddy/jobs/${encodeURIComponent(jobId)}`);
      if (!res.ok) {
        jobError = `Job lookup failed (HTTP ${res.status})`;
        stopPoll();
        return;
      }
      activeJob = await res.json();
      if (activeJob && activeJob.state !== 'running') {
        stopPoll();
        await loadSchedules();
      }
    } catch (e) {
      jobError = 'Polling the job failed: ' + message(e);
      stopPoll();
    } finally {
      pollInFlight = false;
    }
  }

  // startSendJob posts one send and returns its job id (null when the request was
  // refused for a reason the UI can explain).
  async function startSendJob(
    dataset: string,
    receiverUrl: string,
    source: string
  ): Promise<string | null> {
    try {
      const res = await fetch('/api/buddy/send', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ dataset, receiver: receiverUrl, source })
      });
      if (res.status === 409) {
        jobError = 'A backup of this source is already running. Wait for it to finish, or cancel it first.';
        return null;
      }
      if (res.status === 412) {
        jobError = 'This instance has no backup identity yet — create one above first.';
        return null;
      }
      if (!res.ok) throw new Error(await res.text());
      const started = await res.json();
      return started.jobId;
    } catch (e) {
      jobError = 'Starting the backup failed: ' + message(e);
      return null;
    }
  }

  function trackJob(jobId: string) {
    void pollJob(jobId);
    pollTimer = setInterval(() => pollJob(jobId), 1000);
  }

  async function startSend(event: CustomEvent<{ dataset: string; receiver: string; source: string }>) {
    jobError = '';
    activeJob = null;
    stopPoll();
    starting = true;
    try {
      const jobId = await startSendJob(
        event.detail.dataset,
        event.detail.receiver,
        event.detail.source
      );
      if (jobId) trackJob(jobId);
    } finally {
      starting = false;
    }
  }

  async function backupNow(event: CustomEvent<Schedule>) {
    const schedule = event.detail;
    // Fan out exactly like the schedule does; the progress panel follows the
    // first destination and the rest run in the background.
    jobError = '';
    activeJob = null;
    stopPoll();
    starting = true;
    const receivers =
      schedule.receivers && schedule.receivers.length > 0
        ? schedule.receivers
        : [schedule.receiver];
    const started: string[] = [];
    for (const receiver of receivers) {
      const jobId = await startSendJob(schedule.dataset, receiver, schedule.source);
      if (jobId) started.push(jobId);
    }
    if (started.length > 0) trackJob(started[0]);
    starting = false;
  }

  async function cancelJob() {
    if (!activeJob) return;
    try {
      const res = await fetch(`/api/buddy/jobs/${encodeURIComponent(activeJob.id)}`, {
        method: 'DELETE'
      });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      await pollJob(activeJob.id);
    } catch (e) {
      jobError = 'Cancelling failed: ' + message(e);
    }
  }

  onMount(loadAll);
  onDestroy(stopPoll);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Backups"
    subtitle="Push encrypted backups to a buddy. The buddy stores ciphertext it cannot read."
    breadcrumbs={[{ label: 'System' }, { label: 'Backups' }]}
  />

  <AsyncState
    {loading}
    error={error}
    loadingText="Loading backups..."
    onRetry={loadAll}
  >
    <BackupIdentityCard
      {identity}
      creating={creatingIdentity}
      copied={copied === 'key'}
      error={identityError}
      on:create={createIdentity}
      on:copy={copyKey}
    />

    <BackupSchedulesCard
      {schedules}
      datasets={sendableDatasets}
      on:refresh={loadSchedules}
      on:backupNow={backupNow}
    />

    <BackupSendCard
      datasets={sendableDatasets}
      {activeJob}
      {jobError}
      {starting}
      on:send={startSend}
      on:cancel={cancelJob}
    />

    <BackupRestoreCard />

    <BackupReceiverCard
      {receiver}
      missing={receiverMissing}
      error={receiverError}
      on:retry={loadReceiver}
    />
  </AsyncState>
</div>
