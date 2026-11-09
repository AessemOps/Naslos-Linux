  import { writable } from 'svelte/store';

  export interface AppUpdate {
    name: string;
    installedVersion: string;
    latestVersion: string;
  }

  export interface AppUpdates {
    count: number;
    apps: AppUpdate[];
  }

  /** Update summary for the Installed tab badge and the Apps sidebar dot. */
  export const appUpdates = writable<AppUpdates>({ count: 0, apps: [] });

  let timer: ReturnType<typeof setInterval> | null = null;
  let inFlight = false;
  let consumers = 0;

  /** Fetch `/api/apps/updates`; deduplicated so overlapping callers share one
   * request. A failure clears the badge instead of leaving a stale dot. */
  export async function refreshAppUpdates(): Promise<void> {
    if (inFlight) return;
    inFlight = true;
    try {
      const res = await fetch('/api/apps/updates');
      if (!res.ok) {
        appUpdates.set({ count: 0, apps: [] });
        return;
      }
      const data = await res.json();
      appUpdates.set({
        count: Number(data?.count) || 0,
        apps: Array.isArray(data?.apps) ? data.apps : []
      });
    } catch {
      appUpdates.set({ count: 0, apps: [] });
    } finally {
      inFlight = false;
    }
  }

  function tick() {
    if (document.visibilityState === 'visible') void refreshAppUpdates();
  }

  /** Start the 60s poll shared by every consumer (Sidebar, Apps page,
   * Installed list). Returns a stop function; the last consumer stops it. */
  export function startAppUpdates(): () => void {
    consumers += 1;
    if (consumers === 1) {
      void refreshAppUpdates();
      timer = setInterval(tick, 60_000);
      document.addEventListener('visibilitychange', tick);
    }
    return () => {
      consumers = Math.max(0, consumers - 1);
      if (consumers === 0) {
        if (timer !== null) {
          clearInterval(timer);
          timer = null;
        }
        document.removeEventListener('visibilitychange', tick);
      }
    };
  }
