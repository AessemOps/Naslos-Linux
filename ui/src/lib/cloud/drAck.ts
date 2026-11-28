// The disaster-recovery acknowledgement gate for cloud backups. The backend
// has no field for it, so it lives in this browser's localStorage only: it is
// a reminder, not an enforced server-side state.

export const DR_ACK_KEY = 'naslos:cloud-backup:dr-ack';

/** Where the recovery requirements are documented (docs/cloud-backup.md). */
export const CLOUD_DOCS_URL =
  'https://github.com/AessemOps/Naslos-Linux/blob/master/docs/cloud-backup.md';

export function readDrAck(): boolean {
  try {
    return localStorage.getItem(DR_ACK_KEY) !== null;
  } catch {
    return false;
  }
}

export function writeDrAck(): void {
  try {
    localStorage.setItem(DR_ACK_KEY, new Date().toISOString());
  } catch {
    // Storage unavailable: the gate reappears on the next visit.
  }
}
