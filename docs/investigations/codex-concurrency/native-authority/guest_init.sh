#!/bin/sh
BB=/bin/busybox
$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs devtmpfs /dev
$BB modprobe virtio_pci 2>/dev/null || true
$BB modprobe virtio_mmio 2>/dev/null || true
$BB modprobe virtio_console 2>/dev/null || true
exec </dev/hvc0 >/dev/hvc0 2>&1
label=unknown
for argument in $($BB cat /proc/cmdline); do
    case "$argument" in relay.label=*) label=${argument#*=} ;; esac
done
export label
$BB mkdir -p /tmp /sys/fs/cgroup
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null || true
$BB mkdir /sys/fs/cgroup/probe 2>/dev/null || true
$BB cat >/tmp/detached.sh <<'CHILD'
#!/bin/sh
BB=/bin/busybox
# A deliberately privileged guest adversary may leave a naive child cgroup.
# It still has no authority outside this VM kernel.
echo $$ >/sys/fs/cgroup/probe/cgroup.procs 2>/dev/null || true
echo $$ >/sys/fs/cgroup/cgroup.procs 2>/dev/null && echo "RELAY_GUEST $label escaped_naive_cgroup"
i=0
while [ "$i" -lt 100 ]; do
    i=$((i+1))
    echo "$i" >>/tmp/detached-effects
    echo "RELAY_GUEST $label detached $i"
    $BB sleep .1
done
CHILD
$BB setsid /bin/sh /tmp/detached.sh </dev/null >/dev/hvc0 2>&1 &
$BB sleep .4
echo 1 >/sys/fs/cgroup/probe/cgroup.kill 2>/dev/null || true
echo "RELAY_GUEST $label cgroup_kill_requested"
i=0
while [ "$i" -lt 90 ]; do
    i=$((i+1))
    echo "$i" >>/tmp/foreground-effects
    echo "RELAY_GUEST $label foreground $i"
    $BB sleep .1
done
$BB poweroff -f
