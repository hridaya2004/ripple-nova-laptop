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
# popups), the tray icon helper rcc-tray (starts with the window, and at login when Settings > Open at login is
# on; GNOME needs its AppIndicator extension to show it, KDE Plasma shows it as is), and
# the module list that loads acpi_call and uniwill-laptop at boot, a udev hwdb entry that turns the touchpad's
# double-tap corner gesture (F24) into the touchpad-toggle key (not on KDE Plasma), and the uniwill-laptop kernel module
# itself, built from vendor/uniwill-laptop (a git subtree of Wer-Wolf/uniwill-laptop) for the running kernel
# into /usr/lib/modules/<kernel>/updates/ (with LLVM=1 when the kernel was built with clang, which needs clang,
# lld and llvm). It has to be rebuilt after a kernel update: run this again.
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
plasma() { [[ ":${XDG_CURRENT_DESKTOP:-}:" == *:KDE:* ]]; }   # installing from a KDE Plasma session?

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
        "$DESTDIR/etc/udev/hwdb.d/61-uniwill-touchpad.hwdb" \
        "$DESTDIR/etc/xdg/autostart/ripple-nova.desktop" \
        "$DESTDIR/etc/xdg/autostart/ripple-nova-tray.desktop" \
        "$DESTDIR"/usr/lib/modules/*/updates/uniwill-laptop.ko
    $SUDO rm -rf "$DESTDIR/usr/local/lib/ripple-nova"   # the app's own open-at-login copy
    live && { systemctl --user stop ripple-nova-tray 2>/dev/null; pkill -x rcc-tray 2>/dev/null; true; }   # the icon helper runs as you (not running is fine)
    live && $SUDO systemctl daemon-reload
    live && $SUDO depmod -a   # forget the out-of-tree uniwill-laptop (the kernel's own one is used again)
    live && { $SUDO systemd-hwdb update && $SUDO udevadm trigger --subsystem-match=input --action=change; true; }   # drop the F24 mapping
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
        # A kernel built with clang (CachyOS's -lto kernels) needs its modules built with clang as well: gcc stops at
        # the kernel's -fsplit-lto-unit. kbuild's LLVM=1 picks clang, ld.lld and the llvm-* tools, and make passes the
        # variable on to the kernel's own make through vendor/uniwill-laptop/Makefile.
        KBUILD_ARGS=
        if grep -qs '^CONFIG_CC_IS_CLANG=y' "/lib/modules/$KREL/build/.config"; then
            for tool in make clang ld.lld llvm-ar llvm-nm llvm-objcopy; do
                command -v "$tool" >/dev/null || die "the kernel $KREL was built with clang, so its modules are too, and '$tool' is missing.
       Arch: sudo pacman -S --needed base-devel clang lld llvm
       or skip the module with ./install.sh --no-module"
            done
            KBUILD_ARGS=LLVM=1
        else
            command -v make >/dev/null && command -v gcc >/dev/null || die "make and gcc are needed to build the uniwill-laptop module (Arch: sudo pacman -S base-devel)"
        fi
        say "building the uniwill-laptop kernel module for $KREL${KBUILD_ARGS:+ ($KBUILD_ARGS)}"
        as_builder make -C vendor/uniwill-laptop $KBUILD_ARGS clean >/dev/null 2>&1 || true
        as_builder make -C vendor/uniwill-laptop $KBUILD_ARGS >/dev/null || die "building the kernel module failed (run: make -C vendor/uniwill-laptop $KBUILD_ARGS)"
        KO=vendor/uniwill-laptop/uniwill-laptop.ko
        [ -f "$KO" ] || die "the build produced no $KO"
    fi
fi

# ---- install
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova.service > "$tmp/unit"
sed "s|@BINDIR@|$BINDIR|g" dist/ripple-nova > "$tmp/launcher"

say "installing"
$SUDO install -Dm755 target/release/rcc "$DESTDIR$BINDIR/rcc"
$SUDO install -Dm755 target/release/rcc-gui "$DESTDIR$BINDIR/rcc-gui"
$SUDO install -Dm755 target/release/rcc-tray "$DESTDIR$BINDIR/rcc-tray"
# Older versions started the tray at every login through this entry; now only Settings > Open at login does.
$SUDO rm -f "$DESTDIR/etc/xdg/autostart/ripple-nova-tray.desktop"
$SUDO install -Dm755 "$tmp/launcher" "$DESTDIR$BINDIR/ripple-nova"
$SUDO install -Dm644 dist/ripple-nova.desktop "$DESTDIR$PREFIX/share/applications/ripple-nova.desktop"
$SUDO install -Dm644 dist/ripple-nova.svg "$DESTDIR$PREFIX/share/icons/hicolor/scalable/apps/ripple-nova.svg"
$SUDO install -Dm644 "$tmp/unit" "$DESTDIR/etc/systemd/system/$UNIT"
$SUDO install -Dm644 dist/modules-load.conf "$DESTDIR/etc/modules-load.d/ripple-nova.conf"
# Older versions forced the module here (force=1); that would now override the driver's own Ripple Nova entry.
$SUDO rm -f "$DESTDIR/etc/modprobe.d/ripple-nova.conf"
# Plasma needs no mapping to act on the gesture: xkeyboard-config makes Ctrl+Super+F24 the touchpad-toggle key and
# Plasma's "Toggle Touchpad" shortcut is bound to it, so it switches the pointer off in software (no LED) on top of
# the service's switch. The mapping would add nothing there, so leave it out (the service reads the plain
# Ctrl+Meta+F24 chord). The shortcut itself is the user's setting: the notes at the end say to clear it.
if plasma; then
    $SUDO rm -f "$DESTDIR/etc/udev/hwdb.d/61-uniwill-touchpad.hwdb"
else
    $SUDO install -Dm644 dist/61-uniwill-touchpad.hwdb "$DESTDIR/etc/udev/hwdb.d/61-uniwill-touchpad.hwdb"
fi
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
    # the touchpad's corner double tap types Ctrl+Meta+F24: apply (or, on Plasma, drop) the F24 mapping now
    $SUDO systemd-hwdb update && $SUDO udevadm trigger --subsystem-match=input --action=change \
        || warn "could not apply the touchpad-toggle key mapping: sudo systemd-hwdb update && sudo udevadm trigger --subsystem-match=input"
    command -v update-desktop-database >/dev/null && $SUDO update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null && $SUDO gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    # "Open at login" keeps its own root-owned copy of the window and a login script (so the build directory
    # is never what runs as root). They go stale when this script installs a newer build: an old copy would
    # start at the next login with its own popups and no control socket. Refresh them from the new binary.
    if [ -e /etc/xdg/autostart/ripple-nova.desktop ]; then
        say "refreshing the open-at-login copy of the window"
        $SUDO "$BINDIR/rcc-gui" --autostart on >/dev/null || warn "could not refresh the open-at-login copy: run  sudo $BINDIR/rcc-gui --autostart on"
    fi
    # The tray helper runs as you. Restart it to pick up the new build, but only when it was running or the login
    # entry is on: with Open at login off nothing but the window starts it.
    if pgrep -x rcc-tray >/dev/null 2>&1 || [ -e /etc/xdg/autostart/ripple-nova.desktop ]; then
        say "restarting the tray icon helper (it waits until the desktop has a tray)"
        systemctl --user stop ripple-nova-tray 2>/dev/null || true
        pkill -x rcc-tray 2>/dev/null || true
        # its own transient user service, not a child of this terminal (closing the terminal killed it)
        systemd-run --user --quiet --collect --unit=ripple-nova-tray "$BINDIR/rcc-tray" 2>/dev/null || setsid -f "$BINDIR/rcc-tray" >/dev/null 2>&1 || true
    fi
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
    window="opens without a password (/etc/sudoers.d/ripple-nova allows only $BINDIR/rcc-gui and a few exact rcc commands)"
else
    window="asks for your password (polkit)"
fi
if plasma; then
    TOUCHPAD_NOTE="
           KDE Plasma switches the pointer off on this gesture too, with no LED, and the two get out of step.
           Do this once: System Settings > Keyboard > Shortcuts > Touchpad: clear \"Toggle Touchpad\".
           If the pointer is off after that, turn the touchpad on in System Settings > Mouse & Touchpad."
else
    TOUCHPAD_NOTE=" (F24 is mapped to the touchpad-toggle key:
           /etc/udev/hwdb.d/61-uniwill-touchpad.hwdb)"
fi
cat <<MSG

Installed.
  daemon   ripple-nova.service (root), follows the mode button   journalctl -u $UNIT
  window   ripple-nova   (menu: Ripple Control Center), $window
  tray     rcc-tray, started by the window; Settings > Open at login starts it at login (with the window, or
           alone with Start minimized). Its menu switches the performance profile.
           KDE Plasma shows it as is; GNOME only with the AppIndicator extension:
           sudo pacman -S gnome-shell-extension-appindicator, then log out and in
  touchpad double-tap its top-left corner to switch it off and on$TOUCHPAD_NOTE
  cli      sudo rcc doctor | status | mode get|set|next
  remove   ./install.sh --uninstall
MSG
