#!/bin/bash
set -e

# NasOS OpenLDAP Entrypoint
# Handles TLS certificate generation, bootstrap, and slapd startup.

SLAPD_CONFIG_DIR="/etc/ldap/slapd.d"
SLAPD_DATA_DIR="/var/lib/ldap"
CERT_DIR="/container/service/slapd/assets/certs"
LDAP_DOMAIN="${LDAP_DOMAIN:-nasos.local}"
LDAP_ORGANISATION="${LDAP_ORGANISATION:-NasOS}"
LDAP_ADMIN_PASSWORD="${LDAP_ADMIN_PASSWORD:-admin}"
LDAP_TLS="${LDAP_TLS:-true}"

# Generate domain components from LDAP_DOMAIN (e.g. nasos.local -> dc=nasos,dc=local)
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

    # Load initial configuration via slapadd
    cat <<EOF | slapadd -n 0 -F "${SLAPD_CONFIG_DIR}"
dn: cn=config
objectClass: olcGlobal
cn: config
olcServerID: 1
olcLogLevel: none
olcTLSCertificateFile: ${CERT_DIR}/ldap.crt
olcTLSCertificateKeyFile: ${CERT_DIR}/ldap.key
olcTLSCACertificateFile: ${CERT_DIR}/ca.crt
olcTLSCipherSuite: HIGH:!aNULL:!MD5
olcTLSProtocolMin: 3.3

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
    by dn="cn=nasos-service,ou=services,${BASE_DN}" write
    by self write
    by anonymous auth
    by * none
olcAccess: {1}to dn.base="" by * read
olcAccess: {2}to *
    by dn="cn=admin,${BASE_DN}" write
    by dn="cn=nasos-service,ou=services,${BASE_DN}" write
    by * read
olcDbIndex: objectClass eq
olcDbIndex: cn,uid eq
olcDbIndex: mail eq
olcDbIndex: memberOf eq
olcDbIndex: entryUUID eq
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
