/**
 * View types for the S3 cloud backup page (`/backups/cloud`). They mirror the
 * frozen WP-1 API contract (`/api/cloud/*`, docs/api.md). Credentials are
 * write-only: the API reports only the `has*` flags, never a value.
 */

export interface Destination {
  id: string;
  name: string;
  endpoint: string;
  region: string;
  bucket: string;
  prefix: string;
  pathStyle: boolean;
  allowInsecureHttp: boolean;
  enabled: boolean;
  credentialsManaged: boolean;
  hasAccessKey: boolean;
  hasSecretKey: boolean;
  hasSessionToken: boolean;
  conditionalCreate: boolean;
  createdAt?: string;
  lastTestAt?: string;
  lastError?: string;
}

/** Body of POST/PUT `/api/cloud/destinations`. A missing or blank credential
 * keeps the stored value on PUT; the UI omits blank fields entirely. */
export interface DestinationInput {
  name: string;
  endpoint: string;
  region: string;
  bucket: string;
  prefix: string;
  pathStyle: boolean;
  allowInsecureHttp: boolean;
  enabled: boolean;
  accessKeyId?: string;
  secretAccessKey?: string;
  sessionToken?: string;
  credentialsSecret?: string;
}

export type TestCategory =
  | 'dns'
  | 'blocked-address'
  | 'tls'
  | 'auth'
  | 'bucket'
  | 'region'
  | 'network'
  | 'unsupported';

export interface TestResult {
  ok: boolean;
  category?: TestCategory | string;
  conditionalCreate: boolean;
}

export type JobKind = 'send' | 'restore' | 'verify';
export type JobState = 'running' | 'succeeded' | 'failed' | 'cancelled';

export interface JobProgress {
  chunk: number;
  totalChunks?: number;
  plainBytes: number;
  uploaded: number;
  skipped: number;
  link?: number;
  links?: number;
}

export interface JobResult {
  chain: string;
  snapshot: string;
  kind: string;
  plainBytes: number;
  pruned?: number | string[];
  missingIndices?: Record<string, number[]>;
  badIndices?: Record<string, number[]>;
}

export interface CloudJob {
  id: string;
  kind: JobKind;
  destination: string;
  source: string;
  dataset?: string;
  state: JobState;
  startedAt: string;
  finishedAt?: string;
  error?: string;
  progress: JobProgress;
  result?: JobResult;
}

export interface Chain {
  chain: string;
  lineage: string;
  kind: 'full' | 'incremental';
  fromSnapshot: string;
  toSnapshot: string;
  fromGuid: string;
  toGuid: string;
  sequence: number;
  createdAt: string;
  chunks: number;
  plainBytes: number;
  raw: boolean;
}

export type Cadence = 'hourly' | 'daily' | 'weekly';

export interface CloudSchedule {
  id: string;
  dataset: string;
  destinations: string[];
  cadence: Cadence;
  runAt?: string;
  keepLineages: number;
  enabled: boolean;
  lastRun?: string;
  lastResult?: string;
  lastError?: string;
  nextRun?: string;
  destinationResults?: Record<string, string>;
}

export interface DatasetSummary {
  name: string;
}

export interface StartedJob {
  jobId: string;
  status?: string;
}

/** Human text for a `/test` failure category (the API reports only the
 * category, never the upstream response). */
export function categoryText(category: string | undefined): string {
  switch (category) {
    case 'dns':
      return 'The endpoint name does not resolve.';
    case 'blocked-address':
      return 'The endpoint resolves to a blocked (private or local) address.';
    case 'tls':
      return 'The TLS handshake or certificate check failed.';
    case 'auth':
      return 'The credentials were rejected.';
    case 'bucket':
      return 'The bucket does not exist or is not accessible.';
    case 'region':
      return 'The region does not match the bucket.';
    case 'network':
      return 'The endpoint could not be reached.';
    case 'unsupported':
      return 'The endpoint does not support a required S3 operation.';
    default:
      return category ? `The test failed (${category}).` : 'The test failed.';
  }
}

/** True when `endpoint` uses plaintext http (needs the insecure-http ack). */
export function isHttpEndpoint(endpoint: string): boolean {
  return /^\s*http:\/\//i.test(endpoint);
}
