#!/usr/bin/env bash
#
# generate-secrets.sh — create the naslos-openldap and naslos-openldap-tls
# secrets for the Naslos OpenLDAP deployment.
#
# Defaults are suitable for a single-node VM/lab deployment only. Rotate the
# passwords before any real use.

set -euo pipefail

NAMESPACE="${NAMESPACE:-naslos}"
LDAP_DOMAIN="${LDAP_DOMAIN:-naslos.local}"

# No default credentials (NAS-010): generate random ones when the caller does
# not supply them, so a fresh install never ships a documented value. Neither is
# printed - read them back from the naslos-openldap Secret.
if ! command -v openssl >/dev/null 2>&1; then
    echo "ERROR: openssl is required to generate the OpenLDAP secrets" >&2
    exit 1
fi
ADMIN_PASSWORD="${ADMIN_PASSWORD:-$(openssl rand -hex 24)}"
SERVICE_PASSWORD="${SERVICE_PASSWORD:-$(openssl rand -hex 24)}"

echo "Generating OpenLDAP secrets in namespace '$NAMESPACE'..."

# --- password secret ---
kubectl create secret generic naslos-openldap \
    --from-literal=admin-password="$ADMIN_PASSWORD" \
    --from-literal=service-password="$SERVICE_PASSWORD" \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

# --- self-signed TLS secret ---
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

openssl req -x509 -newkey rsa:4096 \
    -keyout "$TMPDIR/ldap.key" \
    -out "$TMPDIR/ldap.crt" \
    -days 3650 -nodes \
    -subj "/CN=ldap.${LDAP_DOMAIN}/O=Naslos" \
    -addext "subjectAltName=DNS:ldap.${LDAP_DOMAIN},DNS:naslos-openldap,DNS:localhost,IP:127.0.0.1"

cp "$TMPDIR/ldap.crt" "$TMPDIR/ca.crt"

kubectl create secret generic naslos-openldap-tls \
    --from-file=ca.crt="$TMPDIR/ca.crt" \
    --from-file=ldap.crt="$TMPDIR/ldap.crt" \
    --from-file=ldap.key="$TMPDIR/ldap.key" \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

echo "OpenLDAP secrets created/updated."