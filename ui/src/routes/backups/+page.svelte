<script lang="ts">
  import { onMount, onDestroy } from 'svelte';

  interface Identity {
    exists: boolean;
    name?: string;
    publicKey?: string;
    fingerprint?: string;
    path?: string;
    createdAt?: string;
    warning?: string;
    help?: string;
  }

  interface Schedule {
    id: string;
    dataset: string;
    source: string;
    receiver: string;
    /** Fan-out targets; `receiver` mirrors the first one. */
    receivers?: string[];
    /** Per-buddy outcome of the last run. */
    receiverResults?: Record<string, string>;
    cadence: string;
    runAt?: string;
    pruneKeep?: number;
    enabled: boolean;
    lastRun?: string;
    lastResult?: string;
    lastError?: string;
    nextRun?: string;
  }

  interface BackupJob {
    id: string;
    dataset: string;
    source: string;
    receiver: string;
    state: string;
    startedAt: string;
    finishedAt?: string;
    progress?: { chunk: number; plainBytes: number; sealedBytes: number; uploaded: number; skipped: number };
    incremental?: boolean;
    resumed?: boolean;
    snapshot?: string;
    estimatedBytes?: number;
    result?: { chain: string; chunks: number; uploaded: number; skipped: number; plainBytes: number; sealedBytes: number; incremental: boolean; resumed: boolean; snapshot: string };
    error?: string;
  }

  interface ReceiverStatus {
    enabled: boolean;
    name: string;
    freeBytes: number;
    usedBytes: number;
    enrollmentOpen: boolean;
    peers: Array<{ name: string; fingerprint: string; enabled: boolean }>;
    backups: Array<{ source: string; chain: string; created?: string; chunks?: number; storedBytes?: number }>;
  }

  interface VerifyChain {
    chain: string;
    sha256: string;
    bytes: number;
  }

  let loading = true;
  let error = '';
  let identity: Identity | null = null;
  let identityName = '';
  let creatingIdentity = false;
  let copied = '';

  let schedules: Schedule[] = [];
  let datasets: Array<{ name: string }> = [];
  // A send streams one dataset: the agent refuses the pool root itself
  // ("dataset must be <pool>/<name>"), so only child datasets are offered.
  $: sendableDatasets = datasets.filter((d) => d.name.includes('/'));
  let newSchedule = { dataset: '', receivers: '', source: '', cadence: 'daily', runAt: '02:30', pruneKeep: 7 };
  let savingSchedule = false;

  let adhoc = { dataset: '', receiver: '', source: '' };
  let activeJob: BackupJob | null = null;
  let jobError = '';
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let pollInFlight = false;

  let receiver: ReceiverStatus | null = null;
  let receiverMissing = false;

  let verifyFor: string = '';
  let verifyChains: VerifyChain[] = [];
  let verifying = false;
  let verifyError = '';

  let restore = { source: '', receiver: '', dataset: '', chain: '', force: false, confirm: '' };
  let restoreResult = '';
  let restoreError = '';
  let restoring = false;

  function fmtBytes(n: number): string {
    if (n == null || isNaN(n)) return '—';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(1)} ${units[i]}`;
  }

  function fmtTime(s?: string): string {
    if (!s) return '—';
    const d = new Date(s);
    if (isNaN(d.getTime())) return s;
    return d.toLocaleString();
  }

  async function loadAll() {
    loading = true;
    error = '';
    try {
      const [idRes, schedRes, dsRes, recvRes] = await Promise.all([
        fetch('/api/buddy/identity'),
        fetch('/api/buddy/schedules'),
        fetch('/api/datasets'),
        fetch('/api/buddy/status')
      ]);
      if (idRes.ok) identity = await idRes.json();
      else if (idRes.status !== 401) error = `Identity: HTTP ${idRes.status}`;
      if (schedRes.ok) {
        const data = await schedRes.json();
        schedules = Array.isArray(data.schedules) ? data.schedules : [];
      }
      if (dsRes.ok) {
        const data = await dsRes.json();
        datasets = Array.isArray(data) ? data : [];
      }
      if (recvRes.ok) {
        receiver = await recvRes.json();
        receiverMissing = false;
      } else if (recvRes.status === 503) {
        receiverMissing = true;
      }
    } catch (e) {
      error = 'Failed to load backup state: ' + e;
    } finally {
      loading = false;
    }
  }

  async function createIdentity() {
    creatingIdentity = true;
    try {
      const res = await fetch('/api/buddy/identity', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: identityName })
      });
      if (!res.ok) throw new Error(await res.text());
      identity = await res.json();
      identityName = '';
    } catch (e) {
      error = 'Creating the identity failed: ' + e;
    } finally {
      creatingIdentity = false;
    }
  }

  async function copyKey() {
    if (!identity?.publicKey) return;
    try {
      await navigator.clipboard.writeText(identity.publicKey);
      copied = 'key';
      setTimeout(() => { if (copied === 'key') copied = ''; }, 2000);
    } catch (e) {
      console.error('copy failed', e);
    }
  }

  async function createSchedule() {
    savingSchedule = true;
    error = '';
    // One buddy per line (or comma separated): every one of them gets its own
    // backup run, so a dead buddy fails only itself.
    const receivers = newSchedule.receivers
      .split(/[\n,]+/)
      .map((r) => r.trim())
      .filter((r) => r.length > 0);
    try {
      const res = await fetch('/api/buddy/schedules', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          dataset: newSchedule.dataset,
          receiver: receivers[0] ?? '',
          receivers,
          source: newSchedule.source,
          cadence: newSchedule.cadence,
          runAt: newSchedule.cadence === 'hourly' ? '' : newSchedule.runAt,
          pruneKeep: Number(newSchedule.pruneKeep) || 0
        })
      });
      if (!res.ok) throw new Error(await res.text());
      newSchedule = { dataset: '', receivers: '', source: '', cadence: 'daily', runAt: '02:30', pruneKeep: 7 };
      await loadSchedules();
    } catch (e) {
      error = 'Saving the schedule failed: ' + e;
    } finally {
      savingSchedule = false;
    }
  }

  async function loadSchedules() {
    try {
      const res = await fetch('/api/buddy/schedules');
      if (res.ok) {
        const data = await res.json();
        schedules = Array.isArray(data.schedules) ? data.schedules : [];
      }
    } catch (e) {
      console.error('schedules reload failed', e);
    }
  }

  async function deleteSchedule(id: string) {
    if (!confirm('Delete this schedule? Already-stored backups are kept.')) return;
    try {
      await fetch(`/api/buddy/schedules?id=${encodeURIComponent(id)}`, { method: 'DELETE' });
      await loadSchedules();
    } catch (e) {
      error = 'Deleting the schedule failed: ' + e;
    }
  }

  function stopPoll() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
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
      jobError = 'Polling the job failed: ' + e;
      stopPoll();
    } finally {
      pollInFlight = false;
    }
  }

  // startSendJob posts one send and returns its job id (null when the request was
  // refused for a reason the UI can explain).
  async function startSendJob(dataset: string, receiverUrl: string, source: string): Promise<string | null> {
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
      jobError = 'Starting the backup failed: ' + e;
      return null;
    }
  }

  function trackJob(jobId: string) {
    void pollJob(jobId);
    pollTimer = setInterval(() => pollJob(jobId), 1000);
  }

  async function startSend(dataset: string, receiverUrl: string, source: string) {
    jobError = '';
    activeJob = null;
    stopPoll();
    const jobId = await startSendJob(dataset, receiverUrl, source);
    if (jobId) trackJob(jobId);
  }

  async function backupNow(s: Schedule) {
    // Fan out exactly like the schedule does; the progress panel follows the
    // first destination and the rest run in the background.
    jobError = '';
    activeJob = null;
    stopPoll();
    const receivers = s.receivers && s.receivers.length > 0 ? s.receivers : [s.receiver];
    const started: string[] = [];
    for (const receiver of receivers) {
      const jobId = await startSendJob(s.dataset, receiver, s.source);
      if (jobId) started.push(jobId);
    }
    if (started.length > 0) trackJob(started[0]);
  }

  async function cancelJob() {
    if (!activeJob) return;
    try {
      await fetch(`/api/buddy/jobs/${encodeURIComponent(activeJob.id)}`, { method: 'DELETE' });
      await pollJob(activeJob.id);
    } catch (e) {
      jobError = 'Cancelling failed: ' + e;
    }
  }

  async function verify(s: Schedule) {
    verifying = true;
    verifyError = '';
    verifyChains = [];
    verifyFor = `${s.source} on ${s.receiver}`;
    try {
      const res = await fetch('/api/buddy/restore', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ source: s.source, receiver: s.receiver, verify: true })
      });
      if (!res.ok) throw new Error(await res.text());
      const data = await res.json();
      verifyChains = Array.isArray(data.chains) ? data.chains : [];
    } catch (e) {
      verifyError = 'Verify failed: ' + e;
    } finally {
      verifying = false;
    }
  }

  async function doRestore() {
    restoreError = '';
    restoreResult = '';
    if (restore.confirm.trim() !== restore.dataset.trim() || !restore.dataset) {
      restoreError = 'Type the destination dataset name to confirm — restoring overwrites it.';
      return;
    }
    restoring = true;
    try {
      const res = await fetch('/api/buddy/restore', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          source: restore.source,
          receiver: restore.receiver,
          dataset: restore.dataset,
          chain: restore.chain || undefined,
          force: restore.force
        })
      });
      if (!res.ok) throw new Error(await res.text());
      const data = await res.json();
      const chains = Array.isArray(data.chains) ? data.chains.join(', ') : data.chain;
      restoreResult = `Restored ${restore.source} into ${restore.dataset} (${data.plainBytes} bytes, chains ${chains}).`;
      restore = { source: '', receiver: '', dataset: '', chain: '', force: false, confirm: '' };
    } catch (e) {
      restoreError = 'Restore failed: ' + e;
    } finally {
      restoring = false;
    }
  }

  onMount(loadAll);
  onDestroy(stopPoll);
</script>

<div class="max-w-5xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Backups</h1>
  <p class="text-gray-400 mb-6">Push encrypted backups to a buddy. The buddy stores ciphertext it cannot read.</p>

  {#if error}<div class="p-4 mb-4 rounded-lg border bg-red-900/30 border-red-700 text-red-300"><span>{error}</span></div>{/if}

  {#if loading}
    <p class="text-gray-400">Loading backups...</p>
  {:else}
    <!-- Identity -->
    <div class="card mb-6">
      <h2 class="text-lg font-bold mb-2">This instance</h2>
      {#if identity?.exists}
        <p class="text-sm text-gray-400">Name: <span class="text-gray-200">{identity.name}</span></p>
        <p class="text-sm text-gray-400">Fingerprint: <code class="text-naslos-primary select-all">{identity.fingerprint}</code></p>
        <div class="flex items-center gap-2 mt-2">
          <code class="text-xs bg-naslos-dark border border-naslos-border rounded px-2 py-1 text-gray-300 select-all break-all">{identity.publicKey}</code>
          <button class="text-xs px-2 py-1 rounded border border-naslos-border text-gray-300 hover:text-white" on:click={copyKey}>{copied === 'key' ? 'Copied' : 'Copy'}</button>
        </div>
        <p class="text-xs text-yellow-400/90 mt-2">Back this identity file up somewhere your buddies cannot reach: losing it means losing the ability to read your backups.</p>
      {:else}
        <p class="text-sm text-gray-400 mb-3">No backup identity yet. Its public key is what a buddy authorizes.</p>
        <div class="flex gap-2">
          <input type="text" bind:value={identityName} placeholder="Instance name (optional)" class="input" />
          <button class="btn btn-primary" on:click={createIdentity} disabled={creatingIdentity}>{creatingIdentity ? 'Creating...' : 'Create identity'}</button>
        </div>
      {/if}
    </div>

    <!-- Schedules -->
    <div class="card mb-6">
      <h2 class="text-lg font-bold mb-2">Schedules</h2>
      {#if schedules.length === 0}
        <p class="text-sm text-gray-400 mb-3">No schedules yet. Backups below run on their cadence and prune to the keep-count on success.</p>
      {:else}
        <table class="w-full text-sm table-fixed mb-4">
          <thead><tr class="text-left text-gray-500">
            <th class="w-28 py-1 align-top">Dataset</th>
            <th class="w-40 py-1 align-top">Source</th>
            <th class="w-40 py-1 align-top">Buddy</th>
            <th class="w-24 py-1 align-top">Cadence</th>
            <th class="w-28 py-1 align-top">Last run</th>
            <th class="w-28 py-1 align-top">Next run</th>
            <th class="w-48 py-1 align-top">Actions</th>
          </tr></thead>
          <tbody>
            {#each schedules as s}
              <tr class="border-t border-naslos-border">
                <td class="py-2 pr-2 align-top truncate" title={s.dataset}>{s.dataset}</td>
                <td class="py-2 pr-2 align-top truncate" title={s.source}>{s.source}</td>
                <td class="py-2 pr-2 align-top truncate" title={(s.receivers ?? [s.receiver]).join('\n')}>
                  {s.receivers && s.receivers.length > 1 ? `${s.receivers.length} buddies` : s.receiver}
                  {#if s.receiverResults}
                    <span class="block text-xs text-gray-500">
                      {#each Object.entries(s.receiverResults) as [url, outcome]}
                        <span class="mr-2" title={url}>{outcome === 'ok' ? '✓' : '✗'} {new URL(url).host}</span>
                      {/each}
                    </span>
                  {/if}
                </td>
                <td class="py-2 pr-2 align-top whitespace-nowrap">{s.cadence}{s.runAt ? ` ${s.runAt}` : ''}</td>
                <td class="py-2 pr-2 align-top">{s.lastResult ? `${s.lastResult} (${fmtTime(s.lastRun)})` : 'never'}</td>
                <td class="py-2 pr-2 align-top">{s.enabled ? fmtTime(s.nextRun) : 'disabled'}</td>
                <td class="py-2 align-top">
                  <div class="flex flex-wrap gap-1">
                    <button class="btn btn-secondary text-xs" on:click={() => backupNow(s)}>Back up now</button>
                    <button class="btn btn-secondary text-xs" on:click={() => verify(s)}>Verify</button>
                    <button class="btn btn-danger text-xs" on:click={() => deleteSchedule(s.id)}>Delete</button>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
      <div class="grid grid-cols-2 md:grid-cols-3 gap-2">
        <select bind:value={newSchedule.dataset} class="input">
          <option value="">Dataset…</option>
          {#each sendableDatasets as d}<option value={d.name}>{d.name}</option>{/each}
        </select>
        <textarea bind:value={newSchedule.receivers} placeholder="Buddy URLs, one per line (https://…)" rows="2" class="input"></textarea>
        <input type="text" bind:value={newSchedule.source} placeholder="Source name naslos-a/data" class="input" />
        <select bind:value={newSchedule.cadence} class="input">
          <option value="hourly">Hourly</option>
          <option value="daily">Daily</option>
          <option value="weekly">Weekly</option>
        </select>
        <input type="text" bind:value={newSchedule.runAt} placeholder={newSchedule.cadence === 'weekly' ? 'Mon 02:30 (UTC)' : '02:30 (UTC)'} class="input" disabled={newSchedule.cadence === 'hourly'} />
        <input type="number" bind:value={newSchedule.pruneKeep} min="0" title="Keep newest N chains" class="input" />
      </div>
      <button class="btn btn-primary mt-3" on:click={createSchedule} disabled={savingSchedule}>{savingSchedule ? 'Saving...' : 'Add schedule'}</button>
      {#if verifyFor}
        <div class="mt-3 text-sm">
          <p class="text-gray-400">Verify {verifyFor}:</p>
          {#if verifying}<p class="text-gray-400">Verifying…</p>
          {:else if verifyError}<p class="text-red-300">{verifyError}</p>
          {:else if verifyChains.length > 0}
            <ul class="mt-1 space-y-1">
              {#each verifyChains as c}<li><code class="text-xs text-naslos-primary">{c.chain}</code> <span class="text-gray-400 text-xs">{c.sha256} ({fmtBytes(c.bytes)})</span></li>{/each}
            </ul>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Manual send + progress -->
    <div class="card mb-6">
      <h2 class="text-lg font-bold mb-2">Back up now</h2>
      <div class="grid grid-cols-1 md:grid-cols-3 gap-2">
        <select bind:value={adhoc.dataset} class="input">
          <option value="">Dataset…</option>
          {#each sendableDatasets as d}<option value={d.name}>{d.name}</option>{/each}
        </select>
        <input type="text" bind:value={adhoc.receiver} placeholder="Buddy URL https://…" class="input" />
        <input type="text" bind:value={adhoc.source} placeholder="Source name" class="input" />
      </div>
      <button class="btn btn-primary mt-3" on:click={() => startSend(adhoc.dataset, adhoc.receiver, adhoc.source)}>Start backup</button>
      {#if jobError}<p class="text-sm text-red-300 mt-2">{jobError}</p>{/if}
      {#if activeJob}
        <div class="mt-3 text-sm">
          <p>Job <code class="text-naslos-primary">{activeJob.id}</code>: <span class="font-medium">{activeJob.state}</span>
            {#if activeJob.incremental}<span class="ml-2 text-xs px-2 py-0.5 rounded bg-blue-900/50 text-blue-300">incremental</span>{/if}
            {#if activeJob.resumed || activeJob.result?.resumed}<span class="ml-2 text-xs px-2 py-0.5 rounded bg-purple-900/50 text-purple-300">resumed</span>{/if}
          </p>
          {#if activeJob.state === 'running'}
            <div class="w-full bg-naslos-dark border border-naslos-border rounded h-3 mt-2 overflow-hidden">
              <div class="bg-naslos-primary h-3 transition-all" style={`width: ${activeJob.estimatedBytes ? Math.min(100, ((activeJob.progress?.plainBytes ?? 0) / activeJob.estimatedBytes) * 100) : 8}%`}></div>
            </div>
            <p class="text-xs text-gray-400 mt-1">{fmtBytes(activeJob.progress?.plainBytes ?? 0)} sent · {activeJob.progress?.uploaded ?? 0} uploaded · {activeJob.progress?.skipped ?? 0} skipped</p>
            <button class="btn btn-danger text-xs mt-2" on:click={cancelJob}>Cancel</button>
          {:else if activeJob.result}
            <p class="text-xs text-gray-400 mt-1">Chain {activeJob.result.chain}: {activeJob.result.chunks} chunks, {fmtBytes(activeJob.result.plainBytes)}.</p>
          {:else if activeJob.error}
            <p class="text-sm text-red-300 mt-1">{activeJob.error}</p>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Restore -->
    <div class="card mb-6">
      <h2 class="text-lg font-bold mb-2">Restore</h2>
      <p class="text-xs text-gray-400 mb-3">Restoring overwrites the destination dataset. Type the destination name to confirm.</p>
      <div class="grid grid-cols-1 md:grid-cols-3 gap-2">
        <input type="text" bind:value={restore.source} placeholder="Source name" class="input" />
        <input type="text" bind:value={restore.receiver} placeholder="Buddy URL" class="input" />
        <input type="text" bind:value={restore.dataset} placeholder="Destination dataset" class="input" />
        <input type="text" bind:value={restore.chain} placeholder="Chain (optional, newest by default)" class="input" />
        <label class="flex items-center gap-2 text-sm text-gray-300"><input type="checkbox" bind:checked={restore.force} class="w-4 h-4" /> Force (-F on first stream)</label>
        <input type="text" bind:value={restore.confirm} placeholder="Type destination to confirm" class="input" />
      </div>
      <button class="btn btn-danger mt-3" on:click={doRestore} disabled={restoring}>{restoring ? 'Restoring...' : 'Restore'}</button>
      {#if restoreError}<p class="text-sm text-red-300 mt-2">{restoreError}</p>{/if}
      {#if restoreResult}<p class="text-sm text-green-300 mt-2">{restoreResult}</p>{/if}
    </div>

    <!-- Receiver -->
    <div class="card">
      <h2 class="text-lg font-bold mb-2">This buddy receives</h2>
      {#if receiverMissing}
        <p class="text-sm text-gray-400">Buddy Backup is not configured on this instance. Create the receive dataset and set <code>buddy.receiveHostPath</code> — see docs/buddy-backup.md §5.1.</p>
      {:else if receiver}
        <p class="text-sm text-gray-400">Free: {fmtBytes(receiver.freeBytes)} · Stored: {fmtBytes(receiver.usedBytes)}
          {#if receiver.enrollmentOpen}<span class="ml-2 text-xs px-2 py-0.5 rounded bg-green-900/50 text-green-300">enrollment open</span>{/if}
        </p>
        <h3 class="font-bold mt-3 mb-1 text-sm">Peers</h3>
        {#if receiver.peers.length === 0}<p class="text-sm text-gray-500">No peers authorized.</p>
        {:else}
          <table class="w-full text-sm table-fixed">
            <tbody>{#each receiver.peers as p}<tr class="border-t border-naslos-border"><td class="py-1 pr-2 align-top truncate">{p.name}</td><td class="py-1 align-top truncate text-gray-400">{p.fingerprint}</td></tr>{/each}</tbody>
          </table>
        {/if}
        <h3 class="font-bold mt-3 mb-1 text-sm">Stored backups</h3>
        {#if receiver.backups.length === 0}<p class="text-sm text-gray-500">Nothing stored yet.</p>
        {:else}
          <table class="w-full text-sm table-fixed">
            <thead><tr class="text-left text-gray-500"><th class="py-1 align-top">Source</th><th class="py-1 align-top">Chain</th><th class="py-1 align-top">Stored</th></tr></thead>
            <tbody>{#each receiver.backups as b}<tr class="border-t border-naslos-border"><td class="py-1 pr-2 align-top truncate">{b.source}</td><td class="py-1 pr-2 align-top truncate">{b.chain}</td><td class="py-1 align-top">{fmtBytes(b.storedBytes ?? 0)}</td></tr>{/each}</tbody>
          </table>
        {/if}
      {/if}
    </div>
  {/if}
</div>
