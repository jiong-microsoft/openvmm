#!/bin/busybox sh
set -e

export PATH=/bin

/bin/busybox mount -t proc proc /proc || true
/bin/busybox mount -t sysfs sysfs /sys || true
/bin/busybox --install -s /bin

/bin/busybox echo CCA_VTL0_SHELL_READY

export PS1='/ # '
exec /bin/busybox sh -i
