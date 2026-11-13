#!/usr/bin/env bash
# Build and install Ripple Control Center.
#
#   ./install.sh              build (as you), install with sudo, enable and start the root daemon
#   ./install.sh --no-sudoers same, without the passwordless-window rule (the window then asks polkit)
#   ./install.sh --no-module  same, without building/installing the uniwill-laptop kernel module
#   ./install.sh --uninstall  remove everything this script installed
#   DESTDIR=/dir ./install.sh stage the files under /dir, touching neither the system nor systemd
#
# Installs: rcc, rcc-gui and rcc-tray (PREFIX/bin, default /usr/local), `ripple-nova` (opens the GUI as root:
# without a password through a sudoers rule that allows exactly rcc-gui for you, else through polkit)
# + a menu entry and icon, the systemd service ripple-nova.service (runs `rcc watch` as root: follows
# the mode button, keeps the profile and the button lights in sync, runs the custom fan profile, sends the
# popups), the tray icon helper rcc-tray (autostarts in your session; GNOME needs its AppIndicator extension
# to show it), and
# the module list that loads acpi_call and uniwill-laptop at boot, and the uniwill-laptop kernel module
# itself, built from vendor/uniwill-laptop (a git subtree of Wer-Wolf/uniwill-laptop) for the running kernel
# into /usr/lib/modules/<kernel>/updates/. It has to be rebuilt after a kernel update: run this again.
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
        "$DESTDIR$BINDIR/rcc" "$DESTDIR$BINDIR/rcc-gui" "$DESTDIR$BINDIR/rcc-tray" "$DESTDIR$BINDIR/ripple-nova" \
        "$DESTDIR/etc/systemd/system/$UNIT" \
        "$DESTDIR$PREFIX/share/applications/ripple-nova.desktop" \
        "$DESTDIR$PREFIX/share/icons/hicolor/scalable/apps/ripple-nova.svg" \
        "$DESTDIR/etc/sudoers.d/ripple-nova" \
        "$DESTDIR/etc/modules-load.d/ripple-nova.conf" \
        "$DESTDIR/etc/modprobe.d/ripple-nova.conf" \
        "$DESTDIR/etc/xdg/autostart/ripple-nova.desktop" \
        "$DESTDIR/etc/xdg/autostart/ripple-nova-tray.desktop" \
        "$DESTDIR"/usr/lib/modules/*/updates/uniwill-laptop.ko
    $SUDO rm -rf "$DESTDIR/usr/local/lib/ripple-nova"   # the app's own open-at-login copy
    live && { pkill -x rcc-tray 2>/dev/null || true; }   # the icon helper runs as you (no process is fine)
    live && $SUDO systemctl daemon-reload
    live && $SUDO depmod -a   # forget the out-of-tree uniwill-laptop (the kernel's own one is used again)
    say "removed. Your settings stay in /etc/ripple-nova (delete it to forget them)."
}

if [ "${1:-}" = "--uninstall" ]; then
    uninstall
    exit 0
fi
SUDOERS=1
MODULE=1
for arg in "$@"; do
    case "$arg" in
        --no-sudoers) SUDOERS=0 ;;
        --no-module) MODULE=0 ;;
        *) die "usage: ./install.sh [--no-sudoers] [--no-module] | --uninstall" ;;
    esac
done
KREL=$(uname -r)

# ---- prerequisites
if live; then
    command -v systemctl >/dev/null || die "systemd is required for the daemon"
    modinfo acpi_call >/dev/null 2>&1 || die "kernel module acpi_call not found.
       Arch:          sudo pacman -S acpi_call   (or acpi_call-dkms)
       Debian/Ubuntu: sudo apt install acpi-call-dkms
       other:         https://github.com/mkottman/acpi_call"
    if [ "$MODULE" = 0 ]; then
        modinfo uniwill-laptop >/dev/null 2>&1 || warn "kernel module uniwill-laptop not found (needs Linux >= 6.19): sensors, fans and charge profiles will be missing"
    fi
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

# ---- kernel module: the out-of-tree uniwill-laptop (git subtree), built for the running kernel
KO=
if [ "$MODULE" = 1 ]; then
    kver=${KREL%%[-+]*}
    if [ "$(printf '%s\n7.2.0\n' "$kver" | sort -V | head -1)" != 7.2.0 ]; then
        warn "kernel $KREL is older than 7.2, which vendor/uniwill-laptop needs: keeping the kernel's own uniwill-laptop (--no-module hides this)"
    else
        [ -e "/lib/modules/$KREL/build" ] || die "the kernel headers for $KREL are missing (needed to build the uniwill-laptop module).
       Arch:          sudo pacman -S linux-headers   (it must match the running kernel: $KREL)
       Debian/Ubuntu: sudo apt install linux-headers-\$(uname -r)
       or skip the module with ./install.sh --no-module"
        command -v make >/dev/null && command -v gcc >/dev/null || die "make and gcc are needed to build the uniwill-laptop module (Arch: sudo pacman -S base-devel)"
        say "building the uniwill-laptop kernel module for $KREL"
        as_builder make -C vendor/uniwill-laptop clean >/dev/null 2>&1 || true
        as_builder make -C vendor/uniwill-laptop >/dev/null || die "building the kernel module failed (run: make -C vendor/uniwill-laptop)"
        KO=vendor/uniwill-laptop/uniwill-laptop.ko
        [ -f "$KO" ] || die "the build produced no $KO"
    fi
fi

# ---- install
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova.service > "$tmp/unit"
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova > "$tmp/launcher"
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova-tray.desktop > "$tmp/tray.desktop"

say "installing"
$SUDO install -Dm755 target/release/rcc "$DESTDIR$BINDIR/rcc"
$SUDO install -Dm755 target/release/rcc-gui "$DESTDIR$BINDIR/rcc-gui"
$SUDO install -Dm755 target/release/rcc-tray "$DESTDIR$BINDIR/rcc-tray"
$SUDO install -Dm644 "$tmp/tray.desktop" "$DESTDIR/etc/xdg/autostart/ripple-nova-tray.desktop"
$SUDO install -Dm755 "$tmp/launcher" "$DESTDIR$BINDIR/ripple-nova"
$SUDO install -Dm644 dist/ripple-nova.desktop "$DESTDIR$PREFIX/share/applications/ripple-nova.desktop"
$SUDO install -Dm644 dist/ripple-nova.svg "$DESTDIR$PREFIX/share/icons/hicolor/scalable/apps/ripple-nova.svg"
$SUDO install -Dm644 "$tmp/unit" "$DESTDIR/etc/systemd/system/$UNIT"
$SUDO install -Dm644 dist/modules-load.conf "$DESTDIR/etc/modules-load.d/ripple-nova.conf"
$SUDO install -Dm644 dist/modprobe.conf "$DESTDIR/etc/modprobe.d/ripple-nova.conf"
if [ -n "$KO" ]; then
    # the same as: mkdir -p /usr/lib/modules/$(uname -r)/updates && cp uniwill-laptop.ko there && depmod -a
    $SUDO install -Dm644 "$KO" "$DESTDIR/usr/lib/modules/$KREL/updates/uniwill-laptop.ko"
fi
$SUDO install -dm755 "$DESTDIR/etc/ripple-nova"

# Passwordless window: a sudoers rule for exactly rcc-gui (see dist/ripple-nova.sudoers). Checked with
# visudo first: a broken file in /etc/sudoers.d would lock everybody out of sudo.
PASSWORDLESS=0
if [ "$SUDOERS" = 1 ]; then
    who=${SUDO_USER:-$(id -un)}
    if [ "$who" = root ]; then
        warn "running as root without SUDO_USER: no sudoers rule written (the window asks polkit)"
    else
        sed "s|@BINDIR@|$BINDIR|g; s|@USER@|$who|g" dist/ripple-nova.sudoers > "$tmp/sudoers"
        if live; then
            $SUDO visudo -cf "$tmp/sudoers" >/dev/null || die "the generated sudoers rule does not parse; nothing was written to /etc/sudoers.d"
        fi
        $SUDO install -Dm440 "$tmp/sudoers" "$DESTDIR/etc/sudoers.d/ripple-nova"
        PASSWORDLESS=1
    fi
fi

if live; then
    command -v update-desktop-database >/dev/null && $SUDO update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null && $SUDO gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    say "starting the tray icon helper (it waits until the desktop has a tray)"
    pkill -x rcc-tray 2>/dev/null || true
    setsid -f "$BINDIR/rcc-tray" >/dev/null 2>&1 || true
    say "starting the daemon"
    [ -n "$KO" ] && $SUDO depmod -a
    $SUDO modprobe acpi_call 2>/dev/null || true
    $SUDO modprobe uniwill_laptop 2>/dev/null || true
    if [ -n "$KO" ] && [ -r /sys/module/uniwill_laptop/srcversion ] \
        && [ "$(cat /sys/module/uniwill_laptop/srcversion)" != "$(modinfo -F srcversion "$KO")" ]; then
        warn "the loaded uniwill_laptop module is an older build. Reboot, or: sudo systemctl stop $UNIT && sudo modprobe -r uniwill_laptop && sudo modprobe uniwill_laptop && sudo systemctl start $UNIT"
    fi
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

if [ "$PASSWORDLESS" = 1 ]; then
    window="opens without a password (/etc/sudoers.d/ripple-nova allows only $BINDIR/rcc-gui)"
else
    window="asks for your password (polkit)"
fi
cat <<MSG

Installed.
  daemon   ripple-nova.service (root), follows the mode button   journalctl -u $UNIT
  window   ripple-nova   (menu: Ripple Control Center), $window
  tray     rcc-tray (autostarts at login). GNOME shows it only with the AppIndicator extension:
           sudo pacman -S gnome-shell-extension-appindicator, then log out and in
  cli      sudo rcc doctor | status | mode get|set|next
  remove   ./install.sh --uninstall
MSG
