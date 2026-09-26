// Package providers loads declarative DNS provider definitions from embedded
// YAML defaults plus an optional override directory. A provider describes its
// credential fields (which drive the UI forms and decide what is a Secret), an
// optional cert-manager DNS-01 solver (rendered for a Domain) and an optional
// DDNS driver. Adding a provider that uses a built-in cert-manager solver or a
// built-in DDNS driver is a YAML change, not a Go change.
package providers

import (
	"embed"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

//go:embed builtin/*.yaml
var builtinFS embed.FS

// dns1123Label matches a DNS-1123 label (a provider name is one).
var dns1123Label = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// FieldType is the UI/config type of a credential field.
type FieldType string

const (
	FieldString FieldType = "string"
	FieldBool   FieldType = "bool"
	FieldEnum   FieldType = "enum"
)

// knownDrivers are the DDNS drivers compiled into the API. A provider may only
// name one of these.
var knownDrivers = map[string]bool{
	"ovh":          true,
	"cloudflare":   true,
	"digitalocean": true,
	"godaddy":      true,
	"porkbun":      true,
	"http":         true,
}

// Field is one provider credential/config field.
type Field struct {
	Key      string    `yaml:"key" json:"key"`
	Label    string    `yaml:"label" json:"label"`
	Type     FieldType `yaml:"type" json:"type"`
	Default  string    `yaml:"default,omitempty" json:"default,omitempty"`
	Enum     []string  `yaml:"enum,omitempty" json:"enum,omitempty"`
	Required bool      `yaml:"required,omitempty" json:"required,omitempty"`
	// Secret marks a field whose value is stored in a Kubernetes Secret and is
	// never returned by the API.
	Secret bool `yaml:"secret,omitempty" json:"secret,omitempty"`
	// SecretKey overrides the key the value is written under in the Secret.
	// Defaults to Key. Needed when a cert-manager solver expects a key that is
	// not a valid field identifier (e.g. cloudflare's `api-token`).
	SecretKey string `yaml:"secretKey,omitempty" json:"secretKey,omitempty"`
	// Scope limits where the field is shown: "cert" (domains/certificates),
	// "ddns" (dynamic DNS) or "" for both. It lets a shared provider (e.g. OVH)
	// expose DynHost credentials to DDNS without cluttering the Domains form.
	Scope string `yaml:"scope,omitempty" json:"scope,omitempty"`
	// ShowIf hides the field in a form until another field visible in the same
	// scope has Value (e.g. OVH's ZoneDNS fields only show for mode: api).
	ShowIf *ShowIf `yaml:"showIf,omitempty" json:"showIf,omitempty"`
}

// ShowIf is a conditional-visibility rule for a field.
type ShowIf struct {
	Key   string `yaml:"key" json:"key"`
	Value string `yaml:"value" json:"value"`
}

// SecretKeyOr returns the Secret data key for the field.
func (f *Field) SecretKeyOr() string {
	if f.SecretKey != "" {
		return f.SecretKey
	}
	return f.Key
}

// CertManager is the provider's cert-manager DNS-01 support. A provider either
// declares a solver (with placeholders below) or, for `passthrough`, declares
// that the raw solver comes from the Domain record itself.
//
//	${secret}      -> the Domain's credentials Secret name
//	${cred.<key>}  -> the Domain's ProviderConfig value (or the field default)
type CertManager struct {
	Solver map[string]interface{} `yaml:"solver" json:"solver,omitempty"`
	// Passthrough marks a provider whose DNS-01 solver is supplied inline on the
	// Domain record instead of being rendered from this definition.
	Passthrough bool `yaml:"passthrough,omitempty" json:"passthrough,omitempty"`
}

// DDNS is the provider's dynamic-DNS support.
type DDNS struct {
	Driver   string            `yaml:"driver" json:"driver"`
	Defaults map[string]string `yaml:"defaults,omitempty" json:"defaults,omitempty"`
}

// Provider is one declarative provider definition.
type Provider struct {
	Name        string       `yaml:"name" json:"name"`
	DisplayName string       `yaml:"displayName" json:"displayName"`
	Description string       `yaml:"description,omitempty" json:"description,omitempty"`
	Icon        string       `yaml:"icon,omitempty" json:"icon,omitempty"`
	// APIRights are the provider API permissions an operator must grant for a
	// domain's DNS-01 solver. Informational; rendered as a UI info bubble.
	APIRights   []string     `yaml:"apiRights,omitempty" json:"apiRights,omitempty"`
	Fields      []Field      `yaml:"fields,omitempty" json:"fields"`
	CertManager *CertManager `yaml:"certManager,omitempty" json:"certManager,omitempty"`
	DDNS        *DDNS        `yaml:"ddns,omitempty" json:"ddns,omitempty"`
}

// HasCertManager reports whether the provider can build a domain DNS-01 solver.
func (p *Provider) HasCertManager() bool {
	return p != nil && p.CertManager != nil && len(p.CertManager.Solver) > 0
}

// HasDDNS reports whether the provider can drive a dynamic-DNS entry.
func (p *Provider) HasDDNS() bool {
	return p != nil && p.DDNS != nil && p.DDNS.Driver != ""
}

// IsPassthrough reports whether the provider takes the raw cert-manager solver
// from the Domain record.
func (p *Provider) IsPassthrough() bool {
	return p != nil && p.CertManager != nil && p.CertManager.Passthrough
}

// SupportsCertificates reports whether the provider can be used for a Domain.
func (p *Provider) SupportsCertificates() bool {
	return p.HasCertManager() || p.IsPassthrough()
}

// SecretFields returns the fields whose values live in a Secret.
func (p *Provider) SecretFields() []Field {
	return p.secretFieldsFor("")
}

// ConfigFields returns the non-secret fields.
func (p *Provider) ConfigFields() []Field {
	return p.configFieldsFor("")
}

// FieldsFor returns the fields visible to one scope: shared fields plus those
// scoped to it. An empty scope returns every field.
func (p *Provider) FieldsFor(scope string) []Field {
	return p.fieldsFor(scope)
}

func (p *Provider) fieldsFor(scope string) []Field {
	out := make([]Field, 0, len(p.Fields))
	for _, f := range p.Fields {
		if f.Scope == "" || scope == "" || f.Scope == scope {
			out = append(out, f)
		}
	}
	return out
}

func (p *Provider) secretFieldsFor(scope string) []Field {
	out := make([]Field, 0)
	for _, f := range p.fieldsFor(scope) {
		if f.Secret {
			out = append(out, f)
		}
	}
	return out
}

func (p *Provider) configFieldsFor(scope string) []Field {
	out := make([]Field, 0)
	for _, f := range p.fieldsFor(scope) {
		if !f.Secret {
			out = append(out, f)
		}
	}
	return out
}

// WithDefaults returns config with every missing field filled from its default.
func (p *Provider) WithDefaults(config map[string]string) map[string]string {
	out := make(map[string]string, len(p.Fields))
	for k, v := range config {
		out[k] = v
	}
	for _, f := range p.Fields {
		if _, ok := out[f.Key]; !ok && f.Default != "" {
			out[f.Key] = f.Default
		}
	}
	return out
}

// ValidateField checks a single field value against its type.
func ValidateField(f Field, value string) error {
	if value == "" {
		return nil
	}
	switch f.Type {
	case FieldBool:
		if value != "true" && value != "false" {
			return fmt.Errorf("field %q must be true or false", f.Key)
		}
	case FieldEnum:
		for _, allowed := range f.Enum {
			if value == allowed {
				return nil
			}
		}
		return fmt.Errorf("field %q must be one of %s", f.Key, strings.Join(f.Enum, ", "))
	}
	return nil
}

// FieldResolution is the result of splitting a provider's submitted fields into
// its non-secret config and the secret values to store.
type FieldResolution struct {
	// Config is the resolved non-secret provider config (submitted, else the
	// existing value, else the field default).
	Config map[string]string
	// SecretValues are the new secret values to write, keyed by SecretKeyOr.
	SecretValues map[string]string
	// CredentialFields names every secret field that is set (existing plus
	// submitted), never their values.
	CredentialFields []string
}

// ResolveFields applies one submission to a provider's field model for the
// given scope ("cert" or "ddns"). It is the single place that decides what is
// config, what is a Secret, which fields are required and which values are
// valid, so domains and Dynamic DNS cannot drift.
//
// existingConfig and existingCredentialFields carry the persisted state on an
// update (nil/empty on create); create enables the required-secret check.
func (p *Provider) ResolveFields(scope string, submitted, existingConfig map[string]string, existingCredentialFields []string, create bool) (FieldResolution, error) {
	out := FieldResolution{
		Config:       map[string]string{},
		SecretValues: map[string]string{},
	}
	out.CredentialFields = append(out.CredentialFields, existingCredentialFields...)

	for _, f := range p.configFieldsFor(scope) {
		value, ok := submitted[f.Key]
		switch {
		case ok:
			out.Config[f.Key] = value
		case existingConfig != nil && existingConfig[f.Key] != "":
			out.Config[f.Key] = existingConfig[f.Key]
		case f.Default != "":
			out.Config[f.Key] = f.Default
		}
		if f.Required && strings.TrimSpace(out.Config[f.Key]) == "" {
			return out, fmt.Errorf("field %q is required", f.Key)
		}
		if err := ValidateField(f, out.Config[f.Key]); err != nil {
			return out, err
		}
	}

	for _, f := range p.secretFieldsFor(scope) {
		if value, ok := submitted[f.Key]; ok && strings.TrimSpace(value) != "" {
			out.SecretValues[f.SecretKeyOr()] = value
			out.CredentialFields = addUnique(out.CredentialFields, f.Key)
			continue
		}
		if create && f.Required {
			return out, fmt.Errorf("field %q is required", f.Key)
		}
		if err := ValidateField(f, submitted[f.Key]); err != nil {
			return out, err
		}
	}
	return out, nil
}

// ValidateSolver renders the cert-manager solver and returns any error, so a
// domain create can reject a missing credential as a 400 rather than storing a
// domain that will never issue.
func (p *Provider) ValidateSolver(secretName string, config map[string]string) error {
	if !p.HasCertManager() {
		return nil
	}
	_, err := p.Solver(secretName, config)
	return err
}

func addUnique(list []string, key string) []string {
	for _, item := range list {
		if item == key {
			return list
		}
	}
	return append(list, key)
}

var (
	credPlaceholder   = regexp.MustCompile(`\$\{cred\.([A-Za-z0-9_]+)\}`)
	secretPlaceholder = regexp.MustCompile(`\$\{secret\}`)
)

// Solver renders the provider's cert-manager solver for a Domain, substituting
// the credentials Secret name and the (default-filled) non-secret config.
func (p *Provider) Solver(secretName string, config map[string]string) (map[string]interface{}, error) {
	if !p.HasCertManager() {
		return nil, fmt.Errorf("provider %q does not define a cert-manager solver", p.Name)
	}
	merged := p.WithDefaults(config)
	rendered, err := renderValue(p.CertManager.Solver, secretName, merged)
	if err != nil {
		return nil, fmt.Errorf("provider %q: %w", p.Name, err)
	}
	solver, ok := rendered.(map[string]interface{})
	if !ok {
		return nil, fmt.Errorf("provider %q: solver is not a mapping", p.Name)
	}
	return solver, nil
}

func renderValue(v interface{}, secretName string, config map[string]string) (interface{}, error) {
	switch t := v.(type) {
	case string:
		return renderString(t, secretName, config)
	case map[string]interface{}:
		out := make(map[string]interface{}, len(t))
		for k, item := range t {
			rendered, err := renderValue(item, secretName, config)
			if err != nil {
				return nil, err
			}
			out[k] = rendered
		}
		return out, nil
	case map[string]string:
		out := make(map[string]interface{}, len(t))
		for k, item := range t {
			rendered, err := renderString(item, secretName, config)
			if err != nil {
				return nil, err
			}
			out[k] = rendered
		}
		return out, nil
	case []interface{}:
		out := make([]interface{}, len(t))
		for i, item := range t {
			rendered, err := renderValue(item, secretName, config)
			if err != nil {
				return nil, err
			}
			out[i] = rendered
		}
		return out, nil
	default:
		return v, nil
	}
}

func renderString(s, secretName string, config map[string]string) (string, error) {
	var err error
	out := credPlaceholder.ReplaceAllStringFunc(s, func(match string) string {
		key := credPlaceholder.FindStringSubmatch(match)[1]
		value, ok := config[key]
		if !ok {
			err = fmt.Errorf("missing config value %q", key)
			return match
		}
		return value
	})
	if err != nil {
		return "", err
	}
	if secretPlaceholder.MatchString(out) {
		if secretName == "" {
			return "", fmt.Errorf("solver references ${secret} but no credentials Secret is set")
		}
		out = secretPlaceholder.ReplaceAllString(out, secretName)
	}
	return out, nil
}

// Registry is a loaded set of provider definitions plus any override files that
// failed to load (surfaced by GET /api/providers rather than being fatal).
type Registry struct {
	providers map[string]*Provider
	errs      []string
}

// Load reads the embedded built-ins and then the optional override directory.
// Override files are applied in sorted filename order; a file whose name matches
// a built-in replaces it, a new name adds a provider. An invalid file is skipped
// and recorded, never fatal.
func Load(overrideDir string) *Registry {
	r := &Registry{providers: make(map[string]*Provider)}

	entries, err := builtinFS.ReadDir("builtin")
	if err != nil {
		r.errs = append(r.errs, "reading embedded providers: "+err.Error())
		return r
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		names = append(names, e.Name())
	}
	sort.Strings(names)
	for _, name := range names {
		data, err := builtinFS.ReadFile(filepath.Join("builtin", name))
		if err != nil {
			r.errs = append(r.errs, fmt.Sprintf("built-in %s: %v", name, err))
			continue
		}
		r.add(data, "builtin/"+name)
	}

	if strings.TrimSpace(overrideDir) != "" {
		files, err := os.ReadDir(overrideDir)
		if err != nil {
			if !os.IsNotExist(err) {
				r.errs = append(r.errs, fmt.Sprintf("reading providers dir %s: %v", overrideDir, err))
			}
		} else {
			overrideNames := make([]string, 0, len(files))
			for _, f := range files {
				if f.IsDir() {
					continue
				}
				ext := strings.ToLower(filepath.Ext(f.Name()))
				if ext != ".yaml" && ext != ".yml" {
					continue
				}
				overrideNames = append(overrideNames, f.Name())
			}
			sort.Strings(overrideNames)
			for _, name := range overrideNames {
				full := filepath.Join(overrideDir, name)
				data, err := os.ReadFile(full)
				if err != nil {
					r.errs = append(r.errs, fmt.Sprintf("provider %s: %v", name, err))
					continue
				}
				r.add(data, full)
			}
		}
	}
	return r
}

func (r *Registry) add(data []byte, source string) {
	p, err := Parse(data)
	if err != nil {
		r.errs = append(r.errs, fmt.Sprintf("%s: %v", source, err))
		return
	}
	r.providers[p.Name] = p
}

// Parse decodes and validates one provider document.
func Parse(data []byte) (*Provider, error) {
	var p Provider
	if err := yaml.Unmarshal(data, &p); err != nil {
		return nil, fmt.Errorf("parsing provider YAML: %w", err)
	}
	if err := p.validate(); err != nil {
		return nil, err
	}
	if p.DisplayName == "" {
		p.DisplayName = p.Name
	}
	return &p, nil
}

func (p *Provider) validate() error {
	if !dns1123Label.MatchString(p.Name) {
		return fmt.Errorf("provider name %q is not a DNS-1123 label", p.Name)
	}
	seen := make(map[string]bool, len(p.Fields))
	for i := range p.Fields {
		f := &p.Fields[i]
		if strings.TrimSpace(f.Key) == "" {
			return fmt.Errorf("provider %q: a field has no key", p.Name)
		}
		if seen[f.Key] {
			return fmt.Errorf("provider %q: duplicate field key %q", p.Name, f.Key)
		}
		seen[f.Key] = true
		switch f.Scope {
		case "", "cert", "ddns":
		default:
			return fmt.Errorf("provider %q field %q: unknown scope %q", p.Name, f.Key, f.Scope)
		}
		if f.ShowIf != nil && strings.TrimSpace(f.ShowIf.Key) == "" {
			return fmt.Errorf("provider %q field %q: showIf needs a key", p.Name, f.Key)
		}
		switch f.Type {
		case FieldString:
		case FieldBool:
			if f.Default != "" && f.Default != "true" && f.Default != "false" {
				return fmt.Errorf("provider %q field %q: bool default must be true or false", p.Name, f.Key)
			}
		case FieldEnum:
			if len(f.Enum) == 0 {
				return fmt.Errorf("provider %q field %q: enum field has no values", p.Name, f.Key)
			}
			if f.Default != "" && !contains(f.Enum, f.Default) {
				return fmt.Errorf("provider %q field %q: default %q is not in the enum", p.Name, f.Key, f.Default)
			}
		default:
			return fmt.Errorf("provider %q field %q: unknown type %q", p.Name, f.Key, f.Type)
		}
	}
	if p.DDNS != nil && p.DDNS.Driver != "" && !knownDrivers[p.DDNS.Driver] {
		return fmt.Errorf("provider %q: unknown DDNS driver %q", p.Name, p.DDNS.Driver)
	}
	for i, right := range p.APIRights {
		if strings.TrimSpace(right) == "" {
			return fmt.Errorf("provider %q: apiRights[%d] is empty", p.Name, i)
		}
	}
	return nil
}

func contains(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
}

// Get returns a provider by name.
func (r *Registry) Get(name string) (*Provider, bool) {
	p, ok := r.providers[name]
	return p, ok
}

// List returns providers ordered by name.
func (r *Registry) List() []*Provider {
	out := make([]*Provider, 0, len(r.providers))
	for _, p := range r.providers {
		out = append(out, p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// Errors returns the non-fatal load errors.
func (r *Registry) Errors() []string {
	return append([]string(nil), r.errs...)
}
