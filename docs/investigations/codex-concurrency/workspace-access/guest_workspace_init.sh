#!/bin/sh
BB=/bin/busybox
$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs devtmpfs /dev
$BB modprobe virtio_pci 2>/dev/null || true
$BB modprobe virtio_mmio 2>/dev/null || true
$BB modprobe virtio_console 2>/dev/null || true
$BB modprobe virtiofs
exec </dev/hvc0 >/dev/hvc0 2>&1
$BB mkdir -p /project /draft /tmp
$BB mount -t virtiofs project /project || echo PROJECT_MOUNT_FAILED
$BB mount -t virtiofs draft /draft 2>/dev/null || true
payload=
for arg in $($BB cat /proc/cmdline); do
 case "$arg" in relay.script=*) payload=${arg#*=} ;; esac
done
printf '%s' "$payload" | $BB base64 -d >/tmp/workload.sh
/bin/sh /tmp/workload.sh
code=$?
echo "WORKLOAD_EXIT $code"
$BB poweroff -f
