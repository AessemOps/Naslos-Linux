# Naslos OpenLDAP Server
# Debian bookworm slapd with memberOf overlay, LDAPS, and bootstrap support.
# Designed to run on Talos Linux with ZFS-backed persistence.

FROM debian:trixie-slim

LABEL maintainer="Naslos <team@naslos.dev>"
LABEL description="OpenLDAP server for Naslos identity management"

ENV LDAP_ORGANISATION="Naslos"
ENV LDAP_DOMAIN="naslos.local"
ENV LDAP_BASE_DN="dc=naslos,dc=local"
# NO LDAP_ADMIN_PASSWORD default here (NAS-010 / trivy DS-0031): baking "admin"
# into the image means a deployment that forgets to wire the Secret silently
# runs with a documented credential. entrypoint.sh requires the variable
# (`:?`), and openldap/manifests/statefulset.yaml supplies it from the
# naslos-openldap Secret, so removing the default fails closed.
ENV LDAP_TLS="true"
ENV LDAP_TLS_ENFORCE="false"
ENV LDAP_TLS_VERIFY_CLIENT="never"
ENV LDAP_REPLICATION="false"

# Install slapd and ldap-utils non-interactively.
#
# dist-upgrade pulls the current Debian security pocket over the base image.
# trixie-slim ships whatever versions were current when the tag was pushed, and
# the trivy scan found base-library HIGHs (util-linux, systemd, ncurses, perl)
# that Debian marks as having no fix yet; this step keeps a rebuild current the
# moment the security pocket publishes one, rather than pinning the base lag.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get dist-upgrade -y && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        slapd \
        ldap-utils \
        openssl \
        ca-certificates \
        && rm -rf /var/lib/apt/lists/*

# Debian 13's slapd defaults olcPidFile to /var/run/slapd/slapd.pid and relies on
# systemd-tmpfiles to create that directory. Containers never run tmpfiles.d, so
# slapd exits 1 before it binds with:
#   unable to open pid file "/var/run/slapd/slapd.pid": 2 (No such file or directory)
# Bookworm did not need this. It must come after the slapd package is installed,
# which is what creates the openldap user.
RUN mkdir -p /var/run/slapd && chown openldap:openldap /var/run/slapd

# Create directories for TLS and bootstrap
RUN mkdir -p /container/service/slapd/assets/certs \
             /container/service/slapd/assets/config/bootstrap \
             /var/lib/ldap/backups

# Copy entrypoint and bootstrap data (overlays are configured inline in
# entrypoint.sh, so only the base bootstrap LDIF is needed here)
COPY entrypoint.sh /container/entrypoint.sh
COPY bootstrap.ldif /container/service/slapd/assets/config/bootstrap/10-bootstrap.ldif

RUN chmod +x /container/entrypoint.sh

# LDAPS port
EXPOSE 636

# Data volume
VOLUME ["/var/lib/ldap", "/etc/ldap/slapd.d", "/container/service/slapd/assets/certs"]

ENTRYPOINT ["/container/entrypoint.sh"]
CMD ["/usr/sbin/slapd", "-h", "ldap:/// ldapi:/// ldaps:///", "-u", "openldap", "-g", "openldap", "-d", "0"]
