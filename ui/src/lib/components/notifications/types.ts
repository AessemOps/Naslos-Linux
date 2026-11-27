// Shared notification-channel types, option lists and payload helpers. The GET
// views are presence-only: destinations and credentials are write-only, so the
// forms keep their own drafts and the payload builder only sends a value when
// the operator typed one (or explicitly cleared a stored one).
export interface NtfySettings {
  enabled: boolean;
  category?: string;
  hasServerUrl: boolean;
  hasTopic: boolean;
  hasAuthToken: boolean;
  hasUrlToken: boolean;
  hasEmail: boolean;
  allowInsecureHttp: boolean;
  enabledEvents: string[];
  minSeverity: string;
}

export interface WebhookSettings {
  enabled: boolean;
  category?: string;
  hasUrl: boolean;
  hasUrlToken: boolean;
  hasAuthToken: boolean;
  allowInsecureHttp: boolean;
  enabledEvents: string[];
  minSeverity: string;
}

export interface SmtpSettings {
  enabled: boolean;
  host: string;
  port: number;
  tls: string;
  username: string;
  hasPassword: boolean;
  from: string;
  to: string[];
  enabledEvents: string[];
  minSeverity: string;
}

export interface SyslogSettings {
  enabled: boolean;
  protocol: string;
  host: string;
  port: number;
  facility: string;
  enabledEvents: string[];
  minSeverity: string;
}

export interface EventType {
  id: string;
  label: string;
}

export interface Choice {
  id: string;
  label: string;
}

export const eventTypes: EventType[] = [
  { id: 'zfs_health', label: 'ZFS Pool Health' },
  { id: 'disk_failure', label: 'Disk Failure' },
  { id: 'backup_failure', label: 'Backup Failure' },
  { id: 'backup_success', label: 'Backup Success' },
  { id: 'vm_failed', label: 'VM Failed to Start' },
  { id: 'vm_exited_unexpected', label: 'VM Unexpected Exit' }
];

export const severityLevels: Choice[] = [
  { id: 'info', label: 'Info (all notifications)' },
  { id: 'warning', label: 'Warning and above' },
  { id: 'error', label: 'Error and above' },
  { id: 'critical', label: 'Critical only' }
];

export const tlsModes: Choice[] = [
  { id: 'starttls', label: 'STARTTLS (port 587)' },
  { id: 'implicit', label: 'Implicit TLS / SMTPS (port 465)' },
  { id: 'none', label: 'None (plaintext LAN relay)' }
];

export const syslogFacilities = [
  'kern', 'user', 'mail', 'daemon', 'auth', 'syslog', 'lpr', 'news', 'uucp',
  'cron', 'authpriv', 'ftp', 'ntp', 'audit', 'console', 'clock',
  'local0', 'local1', 'local2', 'local3', 'local4', 'local5', 'local6', 'local7'
];

export const emptyNtfy: NtfySettings = {
  enabled: false,
  hasServerUrl: false,
  hasTopic: false,
  hasAuthToken: false,
  hasUrlToken: false,
  hasEmail: false,
  allowInsecureHttp: false,
  enabledEvents: ['zfs_health', 'disk_failure'],
  minSeverity: 'warning'
};

export const emptyWebhook: WebhookSettings = {
  enabled: false,
  hasUrl: false,
  hasUrlToken: false,
  hasAuthToken: false,
  allowInsecureHttp: false,
  enabledEvents: ['zfs_health', 'disk_failure'],
  minSeverity: 'warning'
};

export const emptySmtp: SmtpSettings = {
  enabled: false,
  host: '',
  port: 587,
  tls: 'starttls',
  username: '',
  hasPassword: false,
  from: '',
  to: [],
  enabledEvents: ['zfs_health', 'disk_failure'],
  minSeverity: 'warning'
};

export const emptySyslog: SyslogSettings = {
  enabled: false,
  protocol: 'udp',
  host: '',
  port: 514,
  facility: 'local0',
  enabledEvents: ['zfs_health', 'disk_failure'],
  minSeverity: 'warning'
};

// A cleared port input is undefined; Number(undefined) is NaN and
// JSON.stringify turns it into null, which the API's u16 rejects. Omit the
// key instead so the server applies its default.
export function portField(raw: unknown): Record<string, number> {
  if (raw === '' || raw === null || raw === undefined) return {};
  const n = Number(raw);
  return Number.isFinite(n) ? { port: n } : {};
}

export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function toggleId(list: string[], id: string): string[] {
  return list.includes(id) ? list.filter((entry) => entry !== id) : [...list, id];
}
