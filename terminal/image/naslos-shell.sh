# Shell setup for the Naslos web terminal (sourced by login shells via /etc/profile).
#
# The container deliberately has no ZFS packages: the host already ships
# zpool/zfs (its extension provides them), and running those through chroot uses
# the versions matching the running kernel module and keeps dataset mounts in the
# host's mount namespace. These wrappers mean `zpool status` works as typed
# instead of requiring the full `chroot /host ...` invocation.
#
# NOTE: the path after `chroot /host` is relative to the *new* root, so it is
# /usr/local/sbin/zpool (inside the host), not /host/usr/local/sbin/zpool.
NASLOS_HOST_ZPOOL=/usr/local/sbin/zpool
NASLOS_HOST_ZFS=/usr/local/sbin/zfs
NASLOS_HOST_WIPEFS=/usr/bin/wipefs

if [ -x "/host${NASLOS_HOST_ZPOOL}" ]; then
    zpool() { chroot /host "${NASLOS_HOST_ZPOOL}" "$@"; }
    zfs() { chroot /host "${NASLOS_HOST_ZFS}" "$@"; }
fi

if [ -x "/host${NASLOS_HOST_WIPEFS}" ]; then
    wipefs() { chroot /host "${NASLOS_HOST_WIPEFS}" "$@"; }
fi

# Prompt that shows this is the NAS shell.
if [ -n "${BASH_VERSION:-}" ]; then
    PS1='\[\e[36m\]naslos\[\e[0m\]:\w\$ '
fi

