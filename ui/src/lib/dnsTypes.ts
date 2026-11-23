import type { ProviderField } from './providerFields';

/** A DNS/DDNS provider summary from `GET /api/providers`. */
export interface DnsProviderView {
  name: string;
  displayName?: string;
  description?: string;
  certManager?: boolean;
  ddns?: boolean;
  apiRights?: string[];
  fields?: ProviderField[];
}

/** One domain as served by `GET /api/domains`. */
export interface DomainView {
  baseDomain: string;
  dnsProvider: string;
  credentialsSecret?: string;
  acmeEmail?: string;
  environment?: string;
  primary?: boolean;
  sso?: boolean;
  lastError?: string;
  appRecordsDdnsEntry?: string | null;
  appRecordsSupported?: boolean;
  solverConfigured?: boolean;
  redactedProviderConfigKeys?: string[];
  providerConfig?: Record<string, string>;
}

/** One DDNS entry as served by `GET /api/ddns`. */
export interface DdnsEntryView {
  id: string;
  provider: string;
  zone: string;
  record: string;
  recordType?: string;
  ttl?: number;
  enabled?: boolean;
  credentialFields?: string[];
  redactedProviderConfigKeys?: string[];
  providerConfig?: Record<string, string>;
}
