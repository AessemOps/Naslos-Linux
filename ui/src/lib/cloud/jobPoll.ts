// Polls one cloud job (`GET /api/cloud/jobs/{id}`) every second until it
// leaves the `running` state. Same pattern as the Buddy page: a 1 s interval,
// an in-flight guard so a slow response never stacks requests, and an explicit
// stop() the owner calls from onDestroy.
//
//   const poller = createJobPoller({ onUpdate: (j) => (job = j), onError: (m) => (err = m) });
//   poller.start(jobId);
//   onDestroy(poller.stop);
import { api, ApiError } from '$lib/api';
import type { CloudJob } from './types';

export interface JobPollerOptions {
  /** Called with every fetched job snapshot, including the final one. */
  onUpdate: (job: CloudJob) => void;
  /** Called once when the job reaches a terminal state. */
  onDone?: (job: CloudJob) => void;
  /** Called when polling fails; polling stops. */
  onError?: (message: string) => void;
  intervalMs?: number;
}

export interface JobPoller {
  start: (jobId: string) => void;
  stop: () => void;
  /** Request cancellation (DELETE) and refresh the job once. */
  cancel: () => Promise<void>;
  readonly jobId: string | null;
}

function jobPath(id: string): string {
  return `/api/cloud/jobs/${encodeURIComponent(id)}`;
}

export function createJobPoller(options: JobPollerOptions): JobPoller {
  const interval = options.intervalMs ?? 1000;
  let timer: ReturnType<typeof setInterval> | null = null;
  let inFlight = false;
  let current: string | null = null;

  function stop() {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
  }

  async function tick(id: string) {
    if (inFlight || current !== id) return;
    inFlight = true;
    try {
      const job = await api<CloudJob>(jobPath(id));
      if (current !== id) return;
      options.onUpdate(job);
      if (job.state !== 'running') {
        stop();
        options.onDone?.(job);
      }
    } catch (e) {
      if (current !== id) return;
      stop();
      const msg =
        e instanceof ApiError && e.status === 404
          ? 'The job is no longer known to the server.'
          : `Polling the job failed: ${e instanceof Error ? e.message : String(e)}`;
      options.onError?.(msg);
    } finally {
      inFlight = false;
    }
  }

  function start(id: string) {
    stop();
    current = id;
    void tick(id);
    timer = setInterval(() => void tick(id), interval);
  }

  async function cancel() {
    if (!current) return;
    const id = current;
    try {
      await api(jobPath(id), { method: 'DELETE' });
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        options.onError?.('The job has already finished and cannot be cancelled.');
      } else if (e instanceof ApiError && e.status === 404) {
        options.onError?.('The job is no longer known to the server.');
      } else {
        options.onError?.(`Cancelling failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    // Refresh once now (the interval keeps running until the job settles).
    await tick(id);
  }

  return {
    start,
    stop,
    cancel,
    get jobId() {
      return current;
    }
  };
}
