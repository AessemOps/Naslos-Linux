/** Shared view types for the Buddy Backup page and its cards. */

export interface Identity {
  exists: boolean;
  name?: string;
  publicKey?: string;
  fingerprint?: string;
  path?: string;
  createdAt?: string;
  warning?: string;
  help?: string;
}

export interface Schedule {
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

export interface BackupJob {
  id: string;
  dataset: string;
  source: string;
  receiver: string;
  state: string;
  startedAt: string;
  finishedAt?: string;
  progress?: {
    chunk: number;
    plainBytes: number;
    sealedBytes: number;
    uploaded: number;
    skipped: number;
  };
  incremental?: boolean;
  resumed?: boolean;
  snapshot?: string;
  estimatedBytes?: number;
  result?: {
    chain: string;
    chunks: number;
    uploaded: number;
    skipped: number;
    plainBytes: number;
    sealedBytes: number;
    incremental: boolean;
    resumed: boolean;
    snapshot: string;
  };
  error?: string;
}

export interface ReceiverStatus {
  enabled: boolean;
  name: string;
  freeBytes: number;
  usedBytes: number;
  enrollmentOpen: boolean;
  peers: Array<{ name: string; fingerprint: string; enabled: boolean }>;
  backups: Array<{ source: string; chain: string; created?: string; chunks?: number; storedBytes?: number }>;
}

export interface VerifyChain {
  chain: string;
  sha256: string;
  bytes: number;
}

export interface DatasetSummary {
  name: string;
}
