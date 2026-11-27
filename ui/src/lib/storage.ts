// Helpers shared by the storage pages (pools, disks, replication). They were
// previously copied into each page; keeping one copy here means a wording or
// parsing fix reaches every storage surface.

/** The encryption fields every dataset listing exposes. */
export interface DatasetEncryption {
  encryption?: string;
  keystatus?: string;
}

/** A dataset is encrypted when ZFS reports a real encryption algorithm.
 * `off` and the legacy `-` both mean plain text. */
export function isEncryptedDataset(dataset: DatasetEncryption): boolean {
  return !!dataset.encryption && dataset.encryption !== 'off' && dataset.encryption !== '-';
}

/** An encrypted dataset whose key is not loaded. Its data is unreadable
 * until an administrator unlocks it. */
export function isLockedDataset(dataset: DatasetEncryption): boolean {
  return isEncryptedDataset(dataset) && dataset.keystatus === 'unavailable';
}

/**
 * Read a response body as JSON with an error that names the operation.
 *
 * The body is read once as text so a proxy misroute (for example nginx serving
 * index.html for `/api/*` with HTTP 200) produces a readable message instead of
 * `SyntaxError: unexpected character at line 1 column 1`.
 */
export async function parseJsonSafe<T = unknown>(res: Response, what: string): Promise<T> {
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`${what} failed: HTTP ${res.status} — ${text.slice(0, 200)}`);
  }
  const contentType = res.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    throw new Error(
      `${what} failed: expected JSON but got "${contentType || 'unknown content type'}" — ${text.slice(0, 200)}`
    );
  }
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new Error(`${what} failed: invalid JSON response — ${text.slice(0, 200)}`);
  }
}

/**
 * Render a byte count as the first binary unit that divides it exactly.
 * The zvol size parser accepts `[0-9]+(\.[0-9]+)?[KMGTPE]?` or plain bytes;
 * pre-filling a rounded listing label can turn into a silent shrink, so the
 * resize form starts from this exact representation.
 */
export function zfsExactSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '';
  const units: Array<[number, string]> = [
    [1024 ** 4, 'T'],
    [1024 ** 3, 'G'],
    [1024 ** 2, 'M'],
    [1024, 'K']
  ];
  for (const [unit, suffix] of units) {
    if (bytes < unit) continue;
    const value = bytes / unit;
    const text = Number.isInteger(value) ? String(value) : value.toFixed(2).replace(/\.?0+$/, '');
    if (Number(text) * unit === bytes) return `${text}${suffix}`;
  }
  return String(bytes);
}
