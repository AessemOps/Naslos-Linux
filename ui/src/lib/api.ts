// Small typed fetch helper for UI code written against the shared API
// contract (docs/api.md). Existing pages keep their direct fetch calls; new
// components and stores use this so error handling is consistent.
//
//   const app = await api<App>('/api/apps/demo');
//   try { await api('/api/apps/demo', { method: 'DELETE' }); }
//   catch (e) { if (e instanceof ApiError) show(e.message); }

/** An HTTP failure. `status` is the response status; `message` is the API's
 * JSON `error`/`message` field when present, otherwise the response text or a
 * generic HTTP status line. */
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

async function errorMessage(res: Response): Promise<string> {
  let text = '';
  try {
    text = await res.text();
  } catch {
    // Ignore: the status line below is still useful.
  }
  const trimmed = text.trim();
  if (trimmed) {
    try {
      const body = JSON.parse(trimmed);
      if (typeof body?.error === 'string' && body.error) return body.error;
      if (typeof body?.message === 'string' && body.message) return body.message;
    } catch {
      // Not JSON: fall through to the plain text.
    }
    return trimmed;
  }
  return `HTTP ${res.status}${res.statusText ? ` ${res.statusText}` : ''}`;
}

/**
 * Fetch `path` and decode JSON. Throws `ApiError` for any non-2xx response.
 * A 204 (or an empty body) resolves to `undefined` cast to `T`.
 *
 * `init.signal` is passed through, so callers can cancel requests; aborting
 * rejects with the browser's `AbortError`, not an `ApiError`.
 */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (!headers.has('Accept')) headers.set('Accept', 'application/json');
  const res = await fetch(path, { ...init, headers });
  if (!res.ok) {
    throw new ApiError(res.status, await errorMessage(res));
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  if (!text) return undefined as T;
  const contentType = res.headers.get('content-type') ?? '';
  if (contentType.includes('application/json')) {
    return JSON.parse(text) as T;
  }
  return text as unknown as T;
}
