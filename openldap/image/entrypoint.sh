#!/bin/bash
set -e

# Naslos OpenLDAP Entrypoint
# Handles TLS certificate generation, bootstrap, and slapd startup.

SLAPD_CONFIG_DIR="/etc/ldap/slapd.d"
SLAPD_DATA_DIR="/var/lib/ldap"
CERT_DIR="/container/service/slapd/assets/certs"
LDAP_DOMAIN="${LDAP_DOMAIN:-naslos.local}"
LDAP_ORGANISATION="${LDAP_ORGANISATION:-Naslos}"
# No default (NAS-010): the manifests always inject this from the
# naslos-openldap Secret (key admin-password). A fallback of "admin" meant a
# standalone `docker run` of this image initialised the directory with a
# documented root password.
LDAP_ADMIN_PASSWORD="${LDAP_ADMIN_PASSWORD:?LDAP_ADMIN_PASSWORD must be set (from the naslos-openldap Secret)}"
LDAP_TLS="${LDAP_TLS:-true}"

# Generate domain components from LDAP_DOMAIN (e.g. naslos.local -> dc=naslos,dc=local)
IFS='.' read -ra DOMAIN_PARTS <<< "$LDAP_DOMAIN"
BASE_DN=""
for part in "${DOMAIN_PARTS[@]}"; do
    if [ -z "$BASE_DN" ]; then
        BASE_DN="dc=${part}"
    else
        BASE_DN="${BASE_DN},dc=${part}"
    fi
done

# Generate TLS certificates if they don't exist
if [ ! -f "${CERT_DIR}/ldap.crt" ] || [ ! -f "${CERT_DIR}/ldap.key" ]; then
    echo "Generating TLS certificates for LDAP..."
    openssl req -x509 -newkey rsa:4096 \
        -keyout "${CERT_DIR}/ldap.key" \
        -out "${CERT_DIR}/ldap.crt" \
        -days 3650 -nodes \
        -subj "/CN=ldap.${LDAP_DOMAIN}/O=${LDAP_ORGANISATION}" \
        -addext "subjectAltName=DNS:ldap.${LDAP_DOMAIN},DNS:localhost,IP:127.0.0.1"

    # Create CA cert (self-signed in this case)
    cp "${CERT_DIR}/ldap.crt" "${CERT_DIR}/ca.crt"

    # Set permissions
    chown -R openldap:openldap "${CERT_DIR}"
    chmod 600 "${CERT_DIR}/ldap.key"
    chmod 644 "${CERT_DIR}/ldap.crt" "${CERT_DIR}/ca.crt"
fi

# Ensure data directory permissions
chown -R openldap:openldap "${SLAPD_DATA_DIR}"
chown -R openldap:openldap "${SLAPD_CONFIG_DIR}"

# Check if slapd has been initialized
if [ ! -f "${SLAPD_CONFIG_DIR}/cn=config/olcDatabase={1}mdb.ldif" ]; then
    echo "Initializing OpenLDAP configuration..."

    # Generate admin password hash
    ADMIN_PW_HASH=$(slappasswd -h "{SSHA}" -s "${LDAP_ADMIN_PASSWORD}")

    # Create the root cn=config entry first (schemas below are added as
    # children of cn=schema,cn=config, so cn=config must exist beforehand).
    # NOTE: no olcTLSCipherSuite here — Debian's OpenLDAP uses GnuTLS, which
    # rejects OpenSSL-style cipher strings ("HIGH:!aNULL:!MD5") and makes
    # slapadd fail on the cn=config entry with an opaque error.
    cat <<EOF | slapadd -n 0 -F "${SLAPD_CONFIG_DIR}"
dn: cn=config
objectClass: olcGlobal
cn: config
olcServerID: 1
olcLogLevel: none
olcTLSCertificateFile: ${CERT_DIR}/ldap.crt
olcTLSCertificateKeyFile: ${CERT_DIR}/ldap.key
olcTLSCACertificateFile: ${CERT_DIR}/ca.crt
olcTLSProtocolMin: 3.3
EOF

    # Load core + cosine + nis + inetorgperson schemas (in this dependency
    # order — nis depends on cosine, inetorgperson depends on cosine). A
    # fresh slapd.d (e.g. an empty PVC on first boot) has NO schema beyond
    # what we explicitly load here — unlike the image's own build-time
    # slapd.d (populated by the slapd postinst script when the image was
    # built), which already has these baked in. Without this, the
    # olcAccess ACLs below fail with 'unknown attr "shadowLastChange" in to
    # clause' because the nis schema (which defines shadowAccount
    # attributes) was never loaded into this fresh config tree.
    for schema in core cosine nis inetorgperson; do
        slapadd -n 0 -F "${SLAPD_CONFIG_DIR}" -l "/etc/ldap/schema/${schema}.ldif"
    done

    # Load the module list, database, and overlay entries via slapadd.
    # NOTE: back_mdb, memberof and refint are all loadable modules on
    # Debian's slapd — they must be loaded via an explicit cn=module{0}
    # entry before any entry that uses their schema/config attributes, or
    # slapadd fails with "str2ad(...): attribute type undefined" (e.g.
    # olcDbIndex for mdb, or olcMemberOfDangling for the memberof overlay).
    # NOTE 2: LDIF continuation ("folded") lines need TWO leading spaces, not
    # one — RFC 2849 strips exactly one leading space/tab as the fold
    # indicator, so a single leading space collapses into the previous line
    # with no separator (e.g. "shadowLastChange" + "by ..." glue into
    # "shadowLastChangeby ...", which slapadd then rejects as an unknown
    # attribute).
    cat <<EOF | slapadd -n 0 -F "${SLAPD_CONFIG_DIR}"
dn: cn=module{0},cn=config
objectClass: olcModuleList
cn: module{0}
olcModulePath: /usr/lib/ldap
olcModuleLoad: back_mdb
olcModuleLoad: memberof
olcModuleLoad: refint

dn: olcDatabase={0}config,cn=config
objectClass: olcDatabaseConfig
olcDatabase: {0}config
olcAccess: {0}to * by dn.exact=gidNumber=0+uidNumber=0,cn=peercred,cn=external,cn=auth manage by * none
olcRootDN: cn=admin,cn=config
olcRootPW: ${ADMIN_PW_HASH}

dn: olcDatabase={1}mdb,cn=config
objectClass: olcDatabaseConfig
objectClass: olcMdbConfig
olcDatabase: {1}mdb
olcDbDirectory: ${SLAPD_DATA_DIR}
olcSuffix: ${BASE_DN}
olcRootDN: cn=admin,${BASE_DN}
olcRootPW: ${ADMIN_PW_HASH}
olcAccess: {0}to attrs=userPassword,shadowLastChange
  by dn="cn=admin,${BASE_DN}" write
  by dn="cn=naslos-service,ou=services,${BASE_DN}" write
  by self write
  by anonymous auth
  by * none
olcAccess: {1}to dn.base="" by * read
olcAccess: {2}to *
  by dn="cn=admin,${BASE_DN}" write
  by dn="cn=naslos-service,ou=services,${BASE_DN}" write
  by * read
olcDbIndex: objectClass eq
olcDbIndex: cn,uid eq
olcDbIndex: mail eq
olcDbIndex: entryUUID eq
# NOTE: no "olcDbIndex: memberOf eq" here on purpose — the memberOf
# attribute type is only registered once the memberof overlay module
# (added below) is loaded, so indexing it in this same slapadd pass fails
# with "str2ad(olcDbIndex): attribute type undefined" / "could not parse
# entry". The overlay still populates/queries memberOf correctly without
# an index; add one later via ldapmodify on cn=config if desired.
olcDbMaxsize: 1073741824

dn: olcOverlay={0}memberof,olcDatabase={1}mdb,cn=config
objectClass: olcConfig
objectClass: olcMemberOf
objectClass: olcOverlayConfig
olcOverlay: {0}memberof
olcMemberOfDangling: ignore
olcMemberOfRefInt: TRUE
olcMemberOfGroupOC: groupOfNames
olcMemberOfMemberAD: member
olcMemberOfMemberOfAD: memberOf

dn: olcOverlay={1}refint,olcDatabase={1}mdb,cn=config
objectClass: olcConfig
objectClass: olcOverlayConfig
objectClass: olcRefintConfig
olcOverlay: {1}refint
olcRefintAttribute: memberof member manager owner seealso
EOF

    chown -R openldap:openldap "${SLAPD_CONFIG_DIR}"

    echo "Loading bootstrap data..."
    # Load bootstrap LDIF files
    for f in /container/service/slapd/assets/config/bootstrap/*.ldif; do
        if [ -f "$f" ]; then
            slapadd -n 1 -l "$f" -F "${SLAPD_CONFIG_DIR}" 2>/dev/null || \
            ldapadd -Y EXTERNAL -H ldapi:/// -f "$f" 2>/dev/null || true
        fi
    done

    chown -R openldap:openldap "${SLAPD_CONFIG_DIR}"
    chown -R openldap:openldap "${SLAPD_DATA_DIR}"
fi

echo "Starting slapd..."
exec "$@"
