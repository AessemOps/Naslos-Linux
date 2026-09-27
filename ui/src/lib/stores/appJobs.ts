import { writable, get } from 'svelte/store';

export type AppJobKind = 'install' | 'upgrade' | 'uninstall';
export type AppJobState = 'running' | 'succeeded' | 'failed';

export interface AppJob {
  id: string;
  kind: AppJobKind;
  app: string;
  state: AppJobState;
  stage: string;
  message?: string;
  startedAt: string;
  finishedAt?: string;
  baseDomain?: string;
  error?: string;
}

export interface AppToast {
  id: string;
  tone: 'success' | 'error';
  title: string;
  detail?: string;
}

/** Tracked app lifecycle jobs (running plus recently finished, until dismissed). */
export const appJobs = writable<AppJob[]>([]);
/** Transient completion toasts. */
export const appToasts = writable<AppToast[]>([]);

const tracked = new Set<string>();
let timer: ReturnType<typeof setInterval> | null = null;
let inFlight = false;

function successLabel(kind: AppJobKind, app: string): string {
  switch (kind) {
    case 'install':
      return `${app} installed`;
    case 'upgrade':
      return `${app} updated`;
    default:
      return `${app} uninstalled`;
  }
}

function failureLabel(kind: AppJobKind, app: string): string {
  switch (kind) {
    case 'install':
      return `Install of ${app} failed`;
    case 'upgrade':
      return `Update of ${app} failed`;
    default:
      return `Uninstall of ${app} failed`;
  }
}

export function dismissToast(id: string) {
  appToasts.update((list) => list.filter((t) => t.id !== id));
}

function toast(job: AppJob) {
  const ok = job.state === 'succeeded';
  const id = `${job.id}-${job.state}`;
  appToasts.update((list) => [
    ...list,
    {
      id,
      tone: ok ? 'success' : 'error',
      title: ok ? successLabel(job.kind, job.app) : failureLabel(job.kind, job.app),
      detail: ok ? undefined : job.error
    }
  ]);
  setTimeout(() => dismissToast(id), ok ? 5000 : 9000);
}

export function dismissAppJob(id: string) {
  tracked.delete(id);
  appJobs.update((list) => list.filter((j) => j.id !== id));
  if (tracked.size === 0) stopPoll();
}

function upsert(job: AppJob) {
  appJobs.update((list) => {
    const idx = list.findIndex((j) => j.id === job.id);
    if (idx === -1) return [job, ...list];
    const copy = list.slice();
    copy[idx] = job;
    return copy;
  });
}

async function pollOnce() {
  if (inFlight) return;
  inFlight = true;
  const ids = Array.from(tracked);
  try {
    const results = await Promise.all(
      ids.map(async (id) => {
        try {
          const res = await fetch(`/api/apps/jobs/${encodeURIComponent(id)}`);
          if (!res.ok) return null;
          return (await res.json()) as AppJob;
        } catch {
          return null;
        }
      })
    );
    for (const job of results) {
      if (!job) continue;
      const previous = get(appJobs).find((j) => j.id === job.id);
      upsert(job);
      if (job.state !== 'running') {
        tracked.delete(job.id);
        // Toast once, on the running -> terminal transition.
        if (!previous || previous.state === 'running') toast(job);
      }
    }
  } finally {
    inFlight = false;
  }
  if (tracked.size === 0) stopPoll();
}

function startPoll() {
  if (timer !== null) return;
  timer = setInterval(() => {
    void pollOnce();
  }, 1000);
}

function stopPoll() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}

/** Track a job id returned by an enqueue (202) response. One shared poller
 * follows every tracked job and stops when none are running. */
export function trackAppJob(jobId: string) {
  if (!jobId) return;
  tracked.add(jobId);
  startPoll();
  void pollOnce();
}
