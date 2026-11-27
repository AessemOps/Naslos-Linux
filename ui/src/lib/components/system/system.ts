// System page types and pure helpers shared by the page and its cards.
export interface VersionInfo {
  naslosVersion: string;
  chartVersion: string;
  talosVersion: string;
  kubernetesVersion: string;
  releaseTalosVersion: string;
  releaseKubernetesVersion: string;
  schematicId: string;
  profile: string;
  upgradable: boolean;
  updatesEnabled: boolean;
}

export interface Candidate {
  version: string;
  notesUrl: string;
  body: string;
  publishedAt: string;
  talosVersion: string;
  kubernetesVersion: string;
  apiImage: string;
}

export interface Updates {
  enabled: boolean;
  upgradable: boolean;
  current: string;
  checkedAt: string;
  source?: string;
  available: Candidate | null;
  stepping: { allowed: boolean; reason: string };
  blocked: string;
  error: string;
}

export interface Step {
  name: string;
  status: string;
  message: string;
  startedAt: string;
  finishedAt: string;
}

export interface Operation {
  id: string;
  op: string;
  from: string;
  to: string;
  phase: string;
  steps: Step[];
  message: string;
  startedAt: string;
  finishedAt: string;
  result: string;
  attempts: number;
  log: string;
}

export type Op = 'naslos' | 'talos' | 'kubernetes';

export const ops: Op[] = ['naslos', 'talos', 'kubernetes'];

export const opTitle: Record<string, string> = {
  naslos: 'Upgrade Naslos',
  talos: 'Upgrade Talos',
  kubernetes: 'Upgrade Kubernetes'
};

export const activePhases = ['pending', 'running', 'rebooting'];

export function isActive(phase?: string): boolean {
  return !!phase && activePhases.includes(phase);
}

export function cmpVersion(a: string, b: string): number {
  const parse = (v: string) => v.replace(/^v/, '').split('.').map((n) => parseInt(n, 10) || 0);
  const x = parse(a);
  const y = parse(b);
  for (let i = 0; i < 3; i += 1) {
    if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) - (y[i] ?? 0);
  }
  return 0;
}

export function disabledReason(
  op: Op,
  version: VersionInfo | null,
  updates: Updates | null,
  operation: Operation | null
): string {
  if (!version) return 'loading';
  if (!version.upgradable) {
    return version.profile === 'installer'
      ? 'installer release facts are incomplete; reinstall from a current pack or use make install-vm'
      : 'this is a make install-vm deployment; apply releases with make install-vm';
  }
  if (isActive(operation?.phase)) return 'an operation is running';
  const candidate = updates?.available;
  if (!candidate) return 'check for updates first';
  if (!updates?.stepping.allowed) {
    return updates?.stepping.reason || 'this release cannot be applied yet';
  }
  if (op === 'naslos') {
    if (version.naslosVersion === candidate.version) return 'Naslos is up to date';
    if (
      version.talosVersion !== version.releaseTalosVersion ||
      version.kubernetesVersion !== version.releaseKubernetesVersion
    ) {
      return 'finish pending system upgrades first';
    }
    return '';
  }
  if (version.naslosVersion !== candidate.version) return 'upgrade Naslos first';
  if (op === 'talos') {
    if (cmpVersion(candidate.talosVersion, version.talosVersion) <= 0) return 'Talos is already current';
    return '';
  }
  if (version.talosVersion !== candidate.talosVersion) return 'upgrade Talos first';
  if (cmpVersion(candidate.kubernetesVersion, version.kubernetesVersion) <= 0) {
    return 'Kubernetes is already current';
  }
  return '';
}

// Release metadata is remote input. Only an absolute https URL may become a
// clickable link; anything else (javascript:, http:, relative) is dropped.
export function safeNotesUrl(value: unknown): string {
  if (typeof value !== 'string') return '';
  try {
    const parsed = new URL(value.trim());
    return parsed.protocol === 'https:' ? parsed.href : '';
  } catch {
    return '';
  }
}
