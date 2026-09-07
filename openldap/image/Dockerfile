# NasOS OpenLDAP Server
# Debian bookworm slapd with memberOf overlay, LDAPS, and bootstrap support.
# Designed to run on Talos Linux with ZFS-backed persistence.

FROM debian:bookworm-slim

LABEL maintainer="NasOS <team@nasos.dev>"
LABEL description="OpenLDAP server for NasOS identity management"

ENV LDAP_ORGANISATION="NasOS"
ENV LDAP_DOMAIN="nasos.local"
ENV LDAP_BASE_DN="dc=nasos,dc=local"
ENV LDAP_ADMIN_PASSWORD="admin"
ENV LDAP_TLS="true"
ENV LDAP_TLS_ENFORCE="false"
ENV LDAP_TLS_VERIFY_CLIENT="never"
ENV LDAP_REPLICATION="false"

# Install slapd and ldap-utils non-interactively
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        slapd \
        ldap-utils \
        openssl \
        ca-certificates \
        && rm -rf /var/lib/apt/lists/*

# Create directories for TLS and bootstrap
RUN mkdir -p /container/service/slapd/assets/certs \
             /container/service/slapd/assets/config/bootstrap \
             /var/lib/ldap/backups

# Copy entrypoint and bootstrap scripts
COPY entrypoint.sh /container/entrypoint.sh
COPY bootstrap.ldif /container/service/slapd/assets/config/bootstrap/10-bootstrap.ldif
COPY memberof.ldif /container/service/slapd/assets/config/bootstrap/20-memberof.ldif
COPY refint.ldif /container/service/slapd/assets/config/bootstrap/30-refint.ldif

RUN chmod +x /container/entrypoint.sh

# LDAPS port
EXPOSE 636

# Data volume
VOLUME ["/var/lib/ldap", "/etc/ldap/slapd.d", "/container/service/slapd/assets/certs"]

ENTRYPOINT ["/container/entrypoint.sh"]
CMD ["/usr/sbin/slapd", "-h", "ldap:/// ldapi:/// ldaps:///", "-u", "openldap", "-g", "openldap", "-d", "0"]
