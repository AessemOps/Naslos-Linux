// Wire and form shapes for the virtual-machine wizard (FR-VMT). The API serves
// a flattened VmSpec plus live fields. The wizard edits a normalized form: the
// server has no cloud-init "enabled" flag (presence is the toggle) and omits an
// empty forwards list, so those two shapes must never be conflated.

export interface VmPortForward {
  host: number;
  dst: number;
}

export interface VmDiskSpec {
  sizeGB: number;
  format: string;
  bus: string;
  sourceImage?: string | null;
}

export interface VmNetSpec {
  mode: string;
  forwards?: VmPortForward[];
  nic?: string | null;
}

export interface VmCloudInitSpec {
  hostname: string;
  user: string;
  sshKeys?: string[];
}

/** One VM as served by `GET /api/virt/vms`. */
export interface VmView {
  name: string;
  pool: string;
  cpu: number;
  memMiB: number;
  autostart: boolean;
  desired: string;
  firmware: string;
  secureBoot?: boolean;
  disk: VmDiskSpec;
  net: VmNetSpec;
  cloudInit?: VmCloudInitSpec | null;
  iso?: string | null;
  state: string;
  pid?: number | null;
  cpuPercent?: number | null;
  memUsedMiB?: number | null;
  vncPort?: number | null;
  unexpectedExit: boolean;
  lastError: string;
}

/** One library entry from `GET /api/virt/images`. */
export interface VmImage {
  name: string;
  sizeBytes?: number;
}

export interface VmCloudInitPayload {
  hostname: string;
  user: string;
  sshKeys: string[];
}

/** The PUT/POST body the wizard submits. */
export interface VmPayload {
  name: string;
  pool: string;
  cpu: number;
  memMiB: number;
  autostart: boolean;
  firmware: string;
  secureBoot: boolean;
  desired: string;
  disk: {
    sizeGB: number;
    format: string;
    bus: string;
    sourceImage: string | null;
  };
  net: {
    mode: string;
    forwards: VmPortForward[];
  };
  iso: string | null;
  cloudInit?: VmCloudInitPayload;
}

/** The wizard's editable shape, distinct from the server spec above. */
export interface VmForm {
  name: string;
  pool: string;
  cpu: number;
  memMiB: number;
  autostart: boolean;
  firmware: string;
  secureBoot: boolean;
  desired: string;
  disk: {
    sizeGB: number;
    format: string;
    bus: string;
    sourceImage: string | null;
  };
  net: {
    mode: string;
    forwards: VmPortForward[];
  };
  cloudInit: {
    enabled: boolean;
    hostname: string;
    user: string;
    sshKeys: string;
  };
  iso: string | null;
  bootMode: 'iso' | 'image';
}
