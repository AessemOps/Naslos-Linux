/// One editable custom environment variable (FR-APP-26). Values live in the
/// app's `naslos-env-<release>` Secret; secret rows load as `null`.
export interface EnvRow {
  name: string;
  value: string | null;
  secret: boolean;
}

/** The request shape `POST/PUT /api/apps` accepts for one env variable. */
export interface EnvRequestRow {
  name: string;
  value: string | null;
  secret: boolean;
}

export const ENV_NAME_RE = /^[A-Za-z_][A-Za-z0-9_.-]*$/;
export const ENV_NAME_MAX = 128;

export function validEnvName(name: string): boolean {
  return name.length > 0 && name.length <= ENV_NAME_MAX && ENV_NAME_RE.test(name);
}

/** A dataset from `GET /api/datasets/registry`. */
export interface ManagedDataset {
  name: string;
  hostPath: string;
  shared: boolean;
  attachedTo?: string[];
}

/** A private dataset attached to another app cannot be offered to this one.
 * Shared datasets are attachable by design (with an acknowledgement). */
export function datasetBlocked(dataset: ManagedDataset, appName: string): boolean {
  return (
    !dataset.shared &&
    (dataset.attachedTo?.length ?? 0) > 0 &&
    !(dataset.attachedTo ?? []).includes(appName)
  );
}

export interface EnvValidationOptions {
  /** The chart advertises the `env` capability. An unsupported chart sends no
   * array at all, which keeps the stored Secret untouched. */
  supported: boolean;
  /** Install mode: every row needs a value. An edit may leave a stored secret
   * blank to keep the current value. */
  requireValues?: boolean;
  /** Names whose value already exists in the Secret (edit mode). */
  storedNames?: Set<string>;
}

export interface EnvValidationResult {
  rows: EnvRequestRow[] | null;
  error: string;
}

/**
 * Validate and normalize the editable env rows into the request payload.
 *
 * This is the single copy of the name/value rules shared by the install and
 * configure forms: non-empty names, the DNS-ish character set, the reserved
 * `NASLOS_` prefix, no duplicates, and a value (except an untouched stored
 * secret in edit mode, which is sent as `null` so the API keeps it).
 *
 * `rows` is null when validation failed; `error` explains why.
 */
export function validateEnvRows(
  env: EnvRow[],
  options: EnvValidationOptions
): EnvValidationResult {
  if (!options.supported) return { rows: [], error: '' };
  const out: EnvRequestRow[] = [];
  const seen = new Set<string>();
  for (const row of env) {
    const name = row.name.trim();
    if (!name) {
      return { rows: null, error: 'Every environment variable needs a name.' };
    }
    if (!validEnvName(name)) {
      return { rows: null, error: `Invalid environment variable name ${name}.` };
    }
    if (name.startsWith('NASLOS_')) {
      return {
        rows: null,
        error: `Environment variable ${name} uses the reserved NASLOS_ prefix.`
      };
    }
    if (seen.has(name)) {
      return { rows: null, error: `Environment variable ${name} is defined twice.` };
    }
    seen.add(name);
    if (row.value === null || row.value === '') {
      const storedSecret = !!options.storedNames?.has(name);
      if (options.requireValues || !(row.secret && storedSecret)) {
        return {
          rows: null,
          error: `Environment variable ${name} needs a value.`
        };
      }
      out.push({ name, value: null, secret: true });
    } else {
      out.push({ name, value: row.value, secret: row.secret });
    }
  }
  return { rows: out, error: '' };
}
