#!/root/busybox sh
set -e

echo "[plane0] start-tmk.sh reached"

mkdir -p /root/mount
mount -t 9p -o trans=virtio cca_mount /root/mount

cd /root/mount
export RUST_BACKTRACE=1

MODE="${1:-linux}"
if [ "$MODE" = "uefi" ]; then
    if [ ! -x ./openvmm ]; then
        echo "[plane0] ERROR: packed OpenVMM binary is missing"
        exit 1
    fi

    if [ ! -f ./uefi-aarch64.bin ]; then
        echo "[plane0] ERROR: packed AArch64 UEFI IGVM is missing"
        exit 1
    fi

    echo "[plane0] Launching OpenVMM with the AArch64 UEFI IGVM..."
    ./openvmm \
        --igvm type=uefi,path=./uefi-aarch64.bin \
        --igvm-vtl2-relocation-type disable
    echo "PASS"
    exit 0
fi

if [ ! -x ./tmk_vmm ]; then
    echo "[plane0] ERROR: packed tmk_vmm binary is missing"
    exit 1
fi

if [ ! -f ./Image ]; then
    echo "[plane0] ERROR: packed AArch64 Linux Image is missing"
    exit 1
fi

if [ ! -f ./vtl0-initramfs.cpio ]; then
    echo "[plane0] ERROR: packed VTL0 Linux initramfs is missing"
    exit 1
fi

echo "[plane0] Launching CCA Plane1 with a direct-booted Linux VTL0..."
tty_settings="$(/root/busybox stty -g)"
restore_tty() {
    /root/busybox stty "$tty_settings"
}
trap restore_tty EXIT
/root/busybox stty raw -echo opost onlcr
./tmk_vmm --hv cca \
    --linux-kernel ./Image \
    --linux-initrd ./vtl0-initramfs.cpio \
    --linux-serial-raw \
    --linux-success-marker CCA_VTL0_SHELL_COMMAND_OK \
    --memory-mb 96
restore_tty
echo "PASS"
