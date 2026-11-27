import { writable } from 'svelte/store';
import { api } from '$lib/api';

/** The release-update summary the API caches after its periodic check. */
export interface SystemUpdates {
  /** A newer release exists (and the API could resolve it). */
  available: boolean;
  /** Version of the candidate release, when one is available. */
  availableVersion: string;
  /** Whether the release may be applied to this node now. */
  steppingAllowed: boolean;
  /** Reason from the API when stepping is blocked; empty when allowed. */
  steppingReason: string;
  /** Running Naslos version reported by the check. */
  current: string;
  /** ISO timestamp of the cached check; empty when never checked. */
  checkedAt: string;
}

const EMPTY: SystemUpdates = {
  available: false,
  availableVersion: '',
  steppingAllowed: false,
  steppingReason: '',
  current: '',
  checkedAt: ''
};

interface UpdatesResponse {
  available?: { version?: string } | null;
  stepping?: { allowed?: boolean; reason?: string };
  current?: string;
  checkedAt?: string;
}

/** Update summary for the sidebar dot and the Settings/System pages. */
export const systemUpdates = writable<SystemUpdates>(EMPTY);

let inFlight: Promise<void> | null = null;

/**
 * Refresh `/api/system/updates` now. Overlapping callers share one request.
 * A failure clears the summary so a stale "update available" dot cannot
 * survive a reboot or an API outage.
 */
export function refreshSystemUpdates(): Promise<void> {
  if (inFlight) return inFlight;
  inFlight = (async () => {
    try {
      const data = await api<UpdatesResponse>('/api/system/updates');
      systemUpdates.set({
        available: Boolean(data?.available),
        availableVersion: data?.available?.version ?? '',
        steppingAllowed: Boolean(data?.stepping?.allowed),
        steppingReason: data?.stepping?.reason ?? '',
        current: data?.current ?? '',
        checkedAt: data?.checkedAt ?? ''
      });
    } catch {
      systemUpdates.set(EMPTY);
    } finally {
      inFlight = null;
    }
  })();
  return inFlight;
}
