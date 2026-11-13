/// One editable custom environment variable (FR-APP-26). Values live in the
/// app's `naslos-env-<release>` Secret; secret rows load as `null`.
export interface EnvRow {
  name: string;
  value: string | null;
  secret: boolean;
}

export const ENV_NAME_RE = /^[A-Za-z_][A-Za-z0-9_.-]*$/;

export function validEnvName(name: string): boolean {
  return name.length > 0 && name.length <= 128 && ENV_NAME_RE.test(name);
}
