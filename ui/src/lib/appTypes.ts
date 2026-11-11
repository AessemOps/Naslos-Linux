export interface AppExposure {
  subdomain: string;
  tls: boolean;
  auth: boolean;
  localOnly: boolean;
  internet: boolean;
  service?: string;
  port?: number;
  scheme?: string;
}

/** The installed-app view returned by `GET /api/apps`. */
export interface AppView {
  name: string;
  namespace: string;
  chart: string;
  status: string;
  url: string;
  orphaned: boolean;
  description?: string;
  chartVersion?: string;
  chartPath?: string;
  source?: string;
  channel?: string;
  datasets?: string[];
  /** Stored Helm values, with catalog secrets masked as `null`. */
  values?: Record<string, unknown>;
  redactedKeys?: string[];
  lastError?: string;
  createdAt?: string | null;
  updatedAt?: string | null;
  exposure: AppExposure;
  displayName: string;
  icon: string;
  iconUrl: string;
  installedVersion: string;
  latestVersion: string;
  latestSource: string;
  latestChannel: string;
  updateAvailable: boolean;
}

/** Per-app namespace quota from `GET /api/apps/{name}/quota`. */
export interface AppResourceQuota {
  available: boolean;
  namespace?: string;
  hard?: {
    'requests.cpu'?: string;
    'requests.memory'?: string;
    'limits.cpu'?: string;
    'limits.memory'?: string;
  };
}

/** Slot accounting from `GET /api/apps/namespaces`. */
export interface NamespacePool {
  provisioned: number;
  limit: number;
  used: number;
  available: number;
}

export interface AppNamespaces {
  baseline: NamespacePool;
  privileged: NamespacePool;
}

/** Pods the drawer's live log stream can attach to (`GET /api/apps/{name}/pods`). */
export interface AppPodContainer {
  name: string;
  ready: boolean;
  restartCount: number;
}

export interface AppPod {
  name: string;
  phase: string;
  startedAt?: string | null;
  containers: AppPodContainer[];
}

export interface AppPods {
  namespace: string;
  pods: AppPod[];
}

/** One row of `GET /api/apps/usage`. */
export interface AppUsage {
  name: string;
  namespace: string;
  pods: number;
  cpuMillicores: number | null;
  memoryBytes: number | null;
  networkRxBytesPerSec: number | null;
  networkTxBytesPerSec: number | null;
}

export interface UsageReport {
  available: boolean;
  sampledAt: string;
  error: string;
  apps: AppUsage[];
}

/** Client-side 5-minute sparkline history for one app. */
export interface AppUsageHistory {
  cpu: number[];
  memory: number[];
  rx: number[];
  tx: number[];
}
