#!/usr/bin/env bash
set -euo pipefail

echo "[realm-launch] Starting..."
date

CCA_DIR="${CCA_DIR:-/cca}"
LKVM="${CCA_DIR}/lkvm"
KERNEL="${KERNEL:-/cca/Image}"
GUESTROOT="/.lkvm/default"
START_TMK="/root/start-tmk.sh"

for i in $(seq 1 30); do
    echo "[realm-launch] waiting for artifacts: $i"
    if [ -x "$LKVM" ] && [ -f "$KERNEL" ]; then
        break
    fi
    sleep 1
done

if [ ! -x "$LKVM" ]; then
    echo "[realm-launch][ERROR] Missing $LKVM"
    exit 1
fi

if [ ! -f "$KERNEL" ]; then
    echo "[realm-launch][ERROR] Missing $KERNEL"
    exit 1
fi

prepare_plane0_hook() {
    echo "[realm-launch] watcher waiting for Plane0 root tree..."

    for i in $(seq 1 300); do
        if [ -d "$GUESTROOT/root" ] && [ -e "$GUESTROOT/virt/init" ]; then
            echo "[realm-launch] Plane0 root tree found"
            break
        fi
        echo "[realm-launch] waiting for Plane0 root tree: $i/300"
        sleep 1
    done

    if [ ! -d "$GUESTROOT/root" ]; then
        echo "[realm-launch][ERROR] Plane0 root tree did not appear"
        return 1
    fi

    echo "[realm-launch] installing busybox..."
    cp /bin/busybox "$GUESTROOT/root/busybox"
    chmod 755 "$GUESTROOT/root/busybox"

    echo "[realm-launch] installing start-tmk.sh..."
    if [ ! -f "$START_TMK" ]; then
        echo "[realm-launch][ERROR] Missing $START_TMK"
        return 1
    fi

    cp "$START_TMK" "$GUESTROOT/root/start-tmk.sh"
    chmod 755 "$GUESTROOT/root/start-tmk.sh"

    od -An -tx1 -N16 "$GUESTROOT/root/start-tmk.sh" >/dev/console 2>&1 || true
    head -1 "$GUESTROOT/root/start-tmk.sh" >/dev/console 2>&1 || true
    ls -l "$GUESTROOT/root/busybox" >/dev/console 2>&1 || true
    head -1 "$GUESTROOT/root/start-tmk.sh" >/dev/console 2>&1 || true
    echo "[realm-launch] Installed $GUESTROOT/root/start-tmk.sh"
}

prepare_plane0_hook &

cd "$CCA_DIR"
echo "[realm-launch] Launching lkvm..."
exec ./lkvm run --realm --disable-sve --irqchip=gicv3-its \
    -c 1 -m 1024 \
    --no-pvtime --force-pci \
    --console virtio \
    --kernel "$KERNEL" \
    --9p /cca/,cca_mount \
    -p "console=hvc0 root=/dev/vda2" \
    --measurement-algo=sha256 \
    --restricted_mem
