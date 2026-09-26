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
