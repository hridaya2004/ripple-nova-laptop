#!/usr/bin/env bash
# Build and install Ripple Control Center.
#
#   ./install.sh              build (as you), install with sudo, enable and start the root daemon
#   ./install.sh --uninstall  remove everything this script installed
#   DESTDIR=/dir ./install.sh stage the files under /dir, touching neither the system nor systemd
#
# Installs: rcc and rcc-gui (PREFIX/bin, default /usr/local), `ripple-nova` (opens the GUI as root
# through polkit) + a menu entry, the systemd service ripple-nova.service (runs `rcc watch` as root:
# follows the mode button and keeps the profile and the button lights in sync), and the module list
# that loads acpi_call and uniwill-laptop at boot.
set -euo pipefail

PREFIX=${PREFIX:-/usr/local}
BINDIR=$PREFIX/bin
DESTDIR=${DESTDIR:-}
UNIT=ripple-nova.service
cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || die "Linux only"

# Root is needed for the install steps only; the build stays yours (a root-owned target/ is a pain).
SUDO=
if [ -z "$DESTDIR" ] && [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null || die "sudo is needed to install (or run this as root)"
    SUDO=sudo
fi
live() { [ -z "$DESTDIR" ]; }   # touching the real system (systemd, modules)?

uninstall() {
    if live; then
        $SUDO systemctl disable --now "$UNIT" 2>/dev/null || true
    fi
    $SUDO rm -f \
        "$DESTDIR$BINDIR/rcc" "$DESTDIR$BINDIR/rcc-gui" "$DESTDIR$BINDIR/ripple-nova" \
        "$DESTDIR/etc/systemd/system/$UNIT" \
        "$DESTDIR$PREFIX/share/applications/ripple-nova.desktop" \
        "$DESTDIR/etc/modules-load.d/ripple-nova.conf" \
        "$DESTDIR/etc/xdg/autostart/ripple-nova.desktop"
    $SUDO rm -rf "$DESTDIR/usr/local/lib/ripple-nova"   # the app's own open-at-login copy
    live && $SUDO systemctl daemon-reload
    say "removed. Your settings stay in /etc/ripple-nova (delete it to forget them)."
}

if [ "${1:-}" = "--uninstall" ]; then
    uninstall
    exit 0
fi
[ $# -eq 0 ] || die "usage: ./install.sh [--uninstall]"

# ---- prerequisites
if live; then
    command -v systemctl >/dev/null || die "systemd is required for the daemon"
    modinfo acpi_call >/dev/null 2>&1 || die "kernel module acpi_call not found.
       Arch:          sudo pacman -S acpi_call   (or acpi_call-dkms)
       Debian/Ubuntu: sudo apt install acpi-call-dkms
       other:         https://github.com/mkottman/acpi_call"
    modinfo uniwill-laptop >/dev/null 2>&1 || warn "kernel module uniwill-laptop not found (needs Linux >= 6.19): sensors, fans and charge profiles will be missing"
fi

# ---- build, as the invoking user
as_builder() {
    if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
        runuser -u "$SUDO_USER" -- env "PATH=$(getent passwd "$SUDO_USER" | cut -d: -f6)/.cargo/bin:$PATH" "$@"
    else
        PATH="$HOME/.cargo/bin:$PATH" "$@"
    fi
}
as_builder cargo --version >/dev/null 2>&1 || die "cargo not found (install Rust: https://rustup.rs)"
say "building (release)"
as_builder cargo build --release --locked

# ---- install
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova.service > "$tmp/unit"
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova > "$tmp/launcher"

say "installing"
$SUDO install -Dm755 target/release/rcc "$DESTDIR$BINDIR/rcc"
$SUDO install -Dm755 target/release/rcc-gui "$DESTDIR$BINDIR/rcc-gui"
$SUDO install -Dm755 "$tmp/launcher" "$DESTDIR$BINDIR/ripple-nova"
$SUDO install -Dm644 dist/ripple-nova.desktop "$DESTDIR$PREFIX/share/applications/ripple-nova.desktop"
$SUDO install -Dm644 "$tmp/unit" "$DESTDIR/etc/systemd/system/$UNIT"
$SUDO install -Dm644 dist/modules-load.conf "$DESTDIR/etc/modules-load.d/ripple-nova.conf"
$SUDO install -dm755 "$DESTDIR/etc/ripple-nova"

if live; then
    command -v update-desktop-database >/dev/null && $SUDO update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    say "starting the daemon"
    $SUDO modprobe acpi_call 2>/dev/null || true
    $SUDO modprobe uniwill-laptop 2>/dev/null || true
    $SUDO systemctl daemon-reload
    $SUDO systemctl enable "$UNIT"
    $SUDO systemctl restart "$UNIT"
    sleep 1
    if systemctl is-active --quiet "$UNIT"; then
        say "ripple-nova.service is running as root"
    else
        warn "the daemon is not running yet: journalctl -u $UNIT -n 20"
    fi
fi

cat <<MSG

Installed.
  daemon   ripple-nova.service (root), follows the mode button   journalctl -u $UNIT
  window   ripple-nova   (menu: Ripple Control Center), asks for your password
  cli      sudo rcc doctor | status | mode get|set|next
  remove   ./install.sh --uninstall
MSG
