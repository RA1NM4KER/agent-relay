#!/bin/sh
BB=/bin/busybox
$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs devtmpfs /dev
$BB modprobe virtio_pci 2>/dev/null || true
$BB modprobe virtio_mmio 2>/dev/null || true
$BB modprobe virtio_console 2>/dev/null || true
exec </dev/hvc0 >/dev/hvc0 2>&1
$BB mkdir -p /work /tmp
payload=
for arg in $($BB cat /proc/cmdline); do
 case "$arg" in relay.script=*) payload=${arg#*=} ;; esac
done
printf '%s' "$payload" | $BB base64 -d >/tmp/codex-operation.sh
# No host files/devices are shared. This experimental shell is intentionally
# guest-root: escaped/detached children still cannot leave the VM kernel.
/bin/sh /tmp/codex-operation.sh
code=$?
echo "RELAY_RESEARCH_GUEST_FOREGROUND_EXIT $code"
# Shell completion closes this entire research execution realm, including
# detached descendants. Background-shell UX parity is not claimed.
$BB poweroff -f
