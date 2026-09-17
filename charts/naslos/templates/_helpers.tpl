{{/*
Shared template helpers.

The auth/ingress values are read through these helpers rather than as
`.Values.auth.disabled`: a release installed before those sections existed (and
upgraded with `helm upgrade --reuse-values`) has no `auth`/`ingress` map at all,
and evaluating `.Values.auth.disabled` on it aborts the upgrade with
"nil pointer evaluating interface {}.disabled". `get`/`index` tolerate the
missing map, and every helper defaults to the secure posture (auth enabled,
ingress off).
*/}}
{{- define "naslos.authDisabled" -}}
{{- $auth := get .Values "auth" -}}
{{- if and $auth (index $auth "disabled") -}}true{{- else -}}false{{- end -}}
{{- end -}}

{{- define "naslos.authEnabled" -}}
{{- if eq (include "naslos.authDisabled" .) "false" -}}true{{- else -}}false{{- end -}}
{{- end -}}

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
