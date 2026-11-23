export interface ProviderField {
  key: string;
  label: string;
  type: string;
  default?: string;
  enum?: string[];
  required?: boolean;
  secret?: boolean;
  scope?: string;
  showIf?: { key: string; value: string };
}

// visibleFields applies a field's showIf rule against the current values. A rule
// whose controlling field is not present in this form's scope is ignored, so a
// field shared with another scope (e.g. OVH's API fields on the Domains form)
// stays visible.
export function visibleFields(
  fields: ProviderField[],
  values: Record<string, string>
): ProviderField[] {
  const keys = new Set(fields.map((f) => f.key));
  return fields.filter((f) => {
    const cond = f.showIf;
    if (!cond || !cond.key) return true;
    if (!keys.has(cond.key)) return true;
    return String(values[cond.key] ?? '') === String(cond.value);
  });
}

/**
 * The initial form values for a provider: the stored safe config plus each
 * non-secret field's default. Secret fields and redacted config keys are never
 * prefilled because the API does not return them.
 */
export function providerDefaults(
  fields: ProviderField[],
  seed: Record<string, string>,
  redactedKeys: string[] = []
): Record<string, string> {
  const redacted = new Set(redactedKeys);
  const next: Record<string, string> = { ...seed };
  for (const f of fields) {
    if (f.secret || redacted.has(f.key)) continue;
    if (next[f.key] === undefined) next[f.key] = f.default ?? '';
  }
  return next;
}

/**
 * The `fields` map for a provider update. Only values the operator changed are
 * sent, so an unrelated save never clears a write-only destination or
 * credential. Secret/redacted values are only emptied when the explicit clear
 * checkbox was used; a blank input keeps the stored value.
 */
export function changedProviderFields(
  fields: ProviderField[],
  values: Record<string, string>,
  initial: Record<string, string>,
  cleared: Record<string, boolean>,
  redactedKeys: string[] = []
): Record<string, string> {
  const redacted = new Set(redactedKeys);
  const out: Record<string, string> = {};
  for (const f of fields) {
    if (cleared[f.key]) {
      out[f.key] = '';
      continue;
    }
    const value = values[f.key];
    if (value === undefined || value === initial[f.key]) continue;
    if (value === '' && (f.secret || redacted.has(f.key))) continue;
    out[f.key] = value;
  }
  return out;
}
