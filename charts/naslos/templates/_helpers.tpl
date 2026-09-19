{{/*
Shared template helpers.

The auth/ingress values are read through these helpers rather than as
`.Values.auth.<key>`: a release installed before those sections existed (and
upgraded with `helm upgrade --reuse-values`) has no `auth`/`ingress` map at all,
and evaluating a missing key on it aborts the upgrade with
"nil pointer evaluating interface {}.<key>". `get`/`index` tolerate the missing
map. Authentication has no opt-out, so there is no `auth.disabled` helper.
*/}}
{{- define "naslos.authProxySecret" -}}
{{- $auth := get .Values "auth" -}}
{{- if $auth -}}{{ index $auth "proxySecret" | default "" }}{{- end -}}
{{- end -}}

{{- define "naslos.authProxySecretName" -}}
{{- $auth := get .Values "auth" -}}
{{- if $auth -}}{{ index $auth "proxySecretName" | default "" }}{{- end -}}
{{- end -}}

{{- define "naslos.authTraefikCIDR" -}}
{{- $auth := get .Values "auth" -}}
{{- $cidr := "" -}}
{{- if $auth -}}{{- $cidr = index $auth "traefikCIDR" | default "" -}}{{- end -}}
{{- if $cidr -}}{{ $cidr }}{{- else -}}10.0.0.0/8{{- end -}}
{{- end -}}

{{- define "naslos.agentTokenSecret" -}}
{{- $agent := get .Values "agent" -}}
{{- if $agent -}}{{ index $agent "tokenSecret" | default "" }}{{- end -}}
{{- end -}}

{{- define "naslos.ingressEnabled" -}}
{{- $ingress := get .Values "ingress" -}}
{{- if and $ingress (index $ingress "enabled") -}}true{{- else -}}false{{- end -}}
{{- end -}}

{{/*
Render an image reference (NAS-022). A digest wins when set, because a tag is
mutable: `helm upgrade` with the same tag and `IfNotPresent` can silently keep
running old code after a retag, while `repo@sha256:...` is what was verified.
The digest may be given with or without the `sha256:` prefix.

Usage: {{ include "naslos.image" .Values.api.image }}
*/}}
{{- define "naslos.image" -}}
{{- $digest := .digest | default "" -}}
{{- if $digest -}}
{{- $normalized := $digest -}}
{{- if not (hasPrefix "sha256:" $digest) -}}
{{- $normalized = printf "sha256:%s" $digest -}}
{{- end -}}
{{- printf "%s@%s" .repository $normalized -}}
{{- else -}}
{{- printf "%s:%s" .repository (.tag | default "0.1.0") -}}
{{- end -}}
{{- end -}}
