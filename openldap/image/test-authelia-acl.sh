#!/bin/sh
# R-010: prove the Authelia bind account is read/auth only.
#
# Run against a directory bootstrapped by this image:
#   LDAP_URI=ldaps://naslos-openldap:636 \
#   LDAP_ADMIN_PW_FILE=/run/secrets/admin-password \
#   LDAP_AUTHELIA_PW_FILE=/run/secrets/authelia-password \
#   LDAP_CA_FILE=/ldap-ca/ca.crt ./test-authelia-acl.sh
#
# Every mutation must fail (ldapmodify exits non-zero) while a search must
# succeed. Exit 0 means the account cannot modify groups, passwords or the
# directory. This is a live-directory drill, not part of the unit test suite.
set -eu

: "${LDAP_URI:?set LDAP_URI (ldaps://host:636)}"
: "${LDAP_ADMIN_PW_FILE:?set LDAP_ADMIN_PW_FILE}"
: "${LDAP_AUTHELIA_PW_FILE:?set LDAP_AUTHELIA_PW_FILE}"
BASE_DN="${BASE_DN:-dc=naslos,dc=local}"
AUTHELIA_DN="${AUTHELIA_DN:-cn=authelia-service,ou=services,${BASE_DN}}"
ADMIN_DN="${ADMIN_DN:-cn=admin,${BASE_DN}}"
CA_OPT=""
if [ -n "${LDAP_CA_FILE:-}" ]; then
  CA_OPT="-o TLS_CACERT=${LDAP_CA_FILE}"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "1. Authelia bind and read"
ldapsearch -x -H "$LDAP_URI" $CA_OPT -D "$AUTHELIA_DN" -y "$LDAP_AUTHELIA_PW_FILE" \
  -b "$BASE_DN" -s sub "(objectClass=dcObject)" dc >/dev/null

echo "2. Authelia must NOT modify a group"
cat > "$tmp/group.ldif" <<LDIF
dn: cn=naslos_users,ou=groups,${BASE_DN}
changetype: modify
add: description
description: must-not-happen
LDIF
if ldapmodify -x -H "$LDAP_URI" $CA_OPT -D "$AUTHELIA_DN" -y "$LDAP_AUTHELIA_PW_FILE" \
  -f "$tmp/group.ldif" >/dev/null 2>&1; then
  echo "FAIL: the Authelia account modified a group" >&2
  exit 1
fi

echo "3. Authelia must NOT change a password"
cat > "$tmp/password.ldif" <<LDIF
dn: cn=naslos-service,ou=services,${BASE_DN}
changetype: modify
replace: userPassword
userPassword: {SSHA}notallowed
LDIF
if ldapmodify -x -H "$LDAP_URI" $CA_OPT -D "$AUTHELIA_DN" -y "$LDAP_AUTHELIA_PW_FILE" \
  -f "$tmp/password.ldif" >/dev/null 2>&1; then
  echo "FAIL: the Authelia account changed a password" >&2
  exit 1
fi

echo "4. The write account still works (control)"
ldapsearch -x -H "$LDAP_URI" $CA_OPT -D "$ADMIN_DN" -y "$LDAP_ADMIN_PW_FILE" \
  -b "$BASE_DN" -s base "(objectClass=*)" dc >/dev/null

echo "OK: cn=authelia-service is read/auth only"
