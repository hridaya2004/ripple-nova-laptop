# Ripple Control Center

A Linux control center for XMG / Uniwill (Tongfang) laptops, written in Rust.

These laptops ship with a Windows-only "Control Center" that switches performance profiles, drives the fans,
lights the keyboard and flips a few platform switches. Ripple Control Center reimplements the parts of it that
matter on Linux: a command line (`rcc`), a small desktop window, a tray icon and a root daemon that keeps the
laptop in the state you chose. It is not a pixel-for-pixel clone of the OEM app; features matter, pixels do not.

It is written by hand with AI assistance (Claude), and every hardware behaviour in it was checked by running it
on a real laptop.

> **Tested on exactly one machine**: a Tongfang GK7N-class laptop (EC project id 16, `GK7NXXR`, DMI reads
> "Standard"), Arch Linux, kernel 7.2, GNOME on Wayland. Other Uniwill laptops may work; nothing here has been
> verified on them. It writes to the embedded controller (EC), so read [the risks](#risks) first.

| Part | What it is |
|---|---|
| `rcc` | command line: `rcc doctor`, `status`, `mode`, `fan`, `rgb`, `battery`, `system`, `touchpad`, `popups`, `ec` (raw EC access, for development) |
| `rcc-gui` | the window (GPUI): profiles, fans, keyboard, toggles, settings. Needs root; started through `ripple-nova` |
| `ripple-nova.service` | root daemon (`rcc watch`): follows the hardware mode button, restores the profile at boot, runs the custom fan profile, sends the popups |
| `rcc-tray` | tray icon (runs as you; started by the window, and at login when *Open at login* is on): a click opens the window, the menu switches the performance profile. GNOME needs the AppIndicator extension |
| `ripple-nova` | launcher for the window (menu entry "Ripple Control Center") |
| `vendor/uniwill-laptop` | the kernel driver the sensors, LEDs and toggles come from, [vendored in this repository](#the-uniwill-laptop-driver) |

## How it fits together

- **The EC** (fans, power limits, mode lights, keyboard colour) is reached through `acpi_call` (`/proc/acpi/call`),
  calling the same ACPI methods the OEM driver uses: `ECRR`/`ECRW` on the `INOU0000` device, or the WMI
  `GetSetULong` method for registers above `0xFFF`. Writes are read back and paced the way the OEM does (6 ms);
  raw writes through `rcc ec write` are limited to the known register map.
- **Sensors, LEDs, battery charge types and the platform toggles** (Fn lock, Super key, ...) come from the
  `uniwill-laptop` kernel driver's sysfs interface.
- **Root.** Both the EC and sysfs writes need root. The daemon runs as a systemd service; the window runs as root
  too, without a password dialog, through [a narrow sudoers rule](#passwordless-sudo).
- **The tray icon** runs as you (a root process cannot own an icon on your session bus). It reads the profile the
  daemon publishes in `/run/ripple-nova-profile` and switches it through that same sudoers rule.
- **Popups** are sent by the daemon, so they work with no window open.

## Status and scope

**Works** (verified on the machine above):

- **Performance profiles**: Office / Gaming / Turbo (the OEM's own mode names; the Windows app labels them Balanced / Enthusiast / Overboost; 0 / 1 / 2 lights on the
  mode button) = PL1/PL2/PL4 through the EC (the stock three are the EC's own default slots: 35/35/145, 55/55/145, 65/65/145 W), GPU cTGP through the kernel driver, plus your own named profiles
  (*Profile 1*, *Profile 2*, …: each starts as a copy of the profile running when you make it, has its own
  PL1/PL2/PL4/cTGP, and can be reset to the stock mode it came from or deleted). The hardware button is followed, the lights follow the profile, and
  the profile the EC is running is recognised at boot.
- **Fans**: Auto (firmware), Boost, and a **Custom fan profile**: two curves (fan 1 = CPU fan, fan 2 = GPU fan), five
  points each at 45/55/65/75/85 °C (dragged on a chart in the window), switchable off any time, failsafes (see
  [fans](#custom-fan-profile)). **Every profile has its own curves**, starting from the Windows app's defaults for its
  mode (Office tops out lower than Gaming and Turbo); "Default preset" goes back to those, and the curves can only be
  changed in Custom mode. Plus an auto-boost threshold in the window.
- **Keyboard backlight** (EC solution): colour (hue/saturation/brightness), static rainbow, off.
- **Platform toggles** (System tab): Fn lock, Super key and the others below. The **Device** tab has the touchpad: a **Touchpad power** switch (off = the touchpad is powered down: no
  pointer, its LED lights, as it does in suspend; bring it back with the toggle or `sudo -n rcc touchpad on|toggle`,
  which the sudoers rule allows without a password, for instance from a keyboard shortcut). AC Auto-Boot, USB
  powershare and the firmware's own Touchpad toggle are also there. Breathing in suspend is a
  lightbar feature: it only shows on a machine whose EC reports a lightbar.
- **Touchpad double tap**: a double tap in the touchpad's top-left corner switches it off and on, with its LED, as on
  Windows. The embedded controller types Left Ctrl + Left Meta + F24 on the internal keyboard (this is what
  `acpi_listen` and `evtest /dev/input/event3` show); `install.sh` installs `/etc/udev/hwdb.d/61-uniwill-touchpad.hwdb`
  (from `dist/`), which turns F24 (scancode `0x76`) into the touchpad-toggle key that GNOME and KDE act on. Found in
  [uniwill-laptop issue 22](https://github.com/Wer-Wolf/uniwill-laptop/issues/22). It works while the desktop has the
  touchpad open; the *Touchpad power* switch above (which powers the touchpad down) cannot be undone by the gesture.
- **Tray**: click the icon to open the window. Its menu has a **Performance profile** shortcut (the current profile
  ticked, "service not running" when the daemon is down) and an **Enable / Disable notifications** item. Without the sudoers rule,
  polkit asks for a password instead.
  The window starts the tray icon too, and so does login when *Open at login* is on.
- **Popups** (any freedesktop notification daemon): when the profile, fan boost or a toggle changes, whoever changed it
  (window, hardware button, Fn key, CLI). Optional: Settings → Notifications → *Show popup*, the tray menu's *Enable / Disable notifications*,
  or `rcc popups on|off`.
- **Window**: dark/light theme, a font field with typeahead over every installed family, an editable hex keyboard
  colour, no password prompt. Closing the window ends it; the tray icon opens it again.
- **Startup**: *Open at login* (Settings) starts the window and the tray icon when you log in; with *Start minimized*
  on, only the tray icon starts and the window opens from it. The service always starts at boot, whatever these say.
- **Battery** tab (and `rcc battery`): the battery charge mode, one of the types the kernel lists in `charge_types`
  (`Trickle`, `Standard`, `Long_Life`). The descriptions use the OEM's words and names, which match the EC values
  (0, 1, 2): *Standard* is full capacity (OEM: High capacity), *Long life* is reduced capacity (OEM: Balanced), *Trickle*
  is stationary use (OEM: Eco, "healthy"; for laptops that stay on external power, reduced even more). The tab also shows
  charge, status, health and the current: with a mode other than Standard the firmware can report Full or Charging
  while no current flows, so trust the current.
- **Graphics** tab (and `rcc gpu`), shown only when [the NVIDIA driver](#the-nvidia-driver) is loaded: the GPU's power
  limit, temperature and clocks, and the core and memory clock offsets (core -200..200 MHz, memory -500..1000 MHz, in
  steps of 10 and 50). They are off until you switch *Apply the offsets* on; off means the GPU's own clocks. The service puts
  them back after a driver reload. cTGP stays per profile (Profiles tab); here you see the power limit it results in.
- **CLI** for all of the above.

**To implement:**

| Feature | State |
|---|---|
| Battery charge *limit* (EC 1977/2000) | parked on purpose; only the three battery charge modes exist |
| EC fan *curve tables* and the OEM per-model `UserFanTables` | blocked: this model has no OEM tables and its EC curve RAM reads empty. The custom fan profile is software-driven instead |
| Touchpad double-tap toggle and its LED | the key mapping is installed (see *Touchpad double tap*); whether the desktop's toggle and the firmware's LED stay in step is still to be checked on the laptop |
| Keyboard: per-zone colours, other effects, a moving rainbow, music mode, saved effects | not started (the EC only gave a static rainbow) |
| Lightbar | this laptop has none (the kernel exposes one anyway; the controls are hidden) |
| Display refresh rate (needs kscreen-doctor, xrandr or wlr-randr), display colour profiles, dGPU power / mux mode | not started. dGPU power needs a verified ACPI `_OFF`/`_ON` pair first: a wrong `_OFF` is risky, so it is not wired |
| CPU thermal offset (TCC, EC 2008 holds the Gaming default, 2009 and 2010 the others) | read-only knowledge; writing it is not done. The stock service only writes it in a variant of this EC (0x786) that this model may not have |
| The OEM's "Graphics Card" options still missing: GPU temperature target, the Dynamic Boost switch and value, "Unlock Configurable TGP", "Enable Automatic GPU Overboost" | The clock offsets are done (Graphics tab). The temperature target is not offered: this GPU refuses to set it (`nvidia-smi -gtt`: not supported; the 87 C is a read-only specification). cTGP and Dynamic Boost are EC registers (0x743 bits, 0x744, 0x746) that the kernel driver already sets up; only the [NVIDIA driver](#the-nvidia-driver) applies them: cTGP moves the GPU power limit (measured 60 W to 70 W for +10 W) and is a per-profile setting, Dynamic Boost is not confirmed on this model. Automatic GPU overboost: no known mechanism |
| OSD, per-app profiles, process control, keyboard remap, gaming monitor, liquid cooling (Oasis) | OEM features, not planned yet |
| Windows backend, MQTT interop with the OEM UI | out of scope for now |

## Requirements

**Hardware and kernel**

- A Uniwill/Tongfang laptop with the `INOU0000` ACPI device (`ls /sys/bus/acpi/devices | grep INOU`).
- Linux **7.2 or newer** to use the bundled driver build (see [the driver](#the-uniwill-laptop-driver)). On older
  kernels `install.sh` keeps the kernel's own `uniwill-laptop` (mainline since 6.19), if it has one, and says so; use `--no-module`
  to skip the module step explicitly.
- With Secure Boot / kernel lockdown, the out-of-tree module must be signed; `acpi_call` has the same limit.

**Packages** (Arch names; checked on Arch only, other distributions need the equivalents)

```sh
sudo pacman -S --needed base-devel git linux-headers acpi_call libnotify polkit rustup
rustup default stable                      # Rust 1.89 or newer
# window (GPUI) runtime libraries, usually present with a desktop:
sudo pacman -S --needed wayland libxkbcommon vulkan-icd-loader fontconfig freetype2
# plus the Vulkan driver for your GPU: vulkan-intel, vulkan-radeon, vulkan-nouveau (or nvidia-utils, see below)
# GNOME only, for the tray icon:
sudo pacman -S --needed gnome-shell-extension-appindicator
```

- `linux-headers` must match the running kernel (`pacman -Q linux-headers` against `uname -r`); a kernel update
  needs a matching headers update and a rerun of `./install.sh`.
- `acpi_call` gives the EC access (`/proc/acpi/call`); on kernels other than Arch's `linux`, use `acpi_call-dkms`.
- `libnotify` (`notify-send`) delivers popups; `util-linux` (`setpriv`, `flock`) and `sudo` are needed at runtime;
  `polkit` is the fallback when the passwordless rule is not installed.
- **GNOME tray**: install the extension, log out and in once, then it is enabled (or
  `gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com`). GNOME has no taskbar; apps with a window
  show in the dash, tray icons only through this extension.

## Install

```sh
git clone <this repository> && cd ripple-nova
./install.sh              # builds as you, installs with sudo
```

It builds the Rust workspace (release), builds the `uniwill-laptop` kernel module for the running kernel, and
installs: `rcc`, `rcc-gui`, `rcc-tray`, `ripple-nova` (to `/usr/local/bin`), the menu entry and icon,
`ripple-nova.service` (enabled and started), a module-load list (`acpi_call`, `uniwill-laptop`), a modprobe option
(`force=1`, see below), a udev hwdb entry for the touchpad's corner gesture (see *Touchpad double tap* below) and the sudoers
rule. It does **not** turn on starting at login: that is *Open at login* in
Settings (and an install removes the always-on tray login entry that older versions put in `/etc/xdg/autostart`).
Options:

| Flag | Effect |
|---|---|
| `--no-sudoers` | skip the passwordless rule (the window then asks polkit for a password) |
| `--no-module` | skip building/installing the kernel module (use the kernel's own driver) |
| `--uninstall` | remove everything the script installed (settings in `/etc/ripple-nova` stay) |
| `DESTDIR=/dir ./install.sh` | stage the files under `/dir`, touching neither the system nor systemd |

Then open **Ripple Control Center** from the menu (or run `ripple-nova`), and check `sudo rcc doctor`.
To have the app start at login, switch on *Open at login* in Settings. Settings live in `/etc/ripple-nova/`.

**Popups and Do Not Disturb**: GNOME keeps banners away while Do Not Disturb is on; the notification still lands
in the tray.

## The uniwill-laptop driver

The sensors, the keyboard and lightbar LEDs, the battery charge types and the platform toggles all come from the
`uniwill-laptop` kernel driver. This repository carries its own copy of it in `vendor/uniwill-laptop`:

- **What it is**: the out-of-tree tree of <https://github.com/Wer-Wolf/uniwill-laptop> (`master`). The driver is
  meant for mainline Linux from 6.19; this tree mainly holds experimental features for testers and needs kernel
  7.2 or newer.
- **How it is vendored**: as a squashed **git subtree**, pinned to upstream commit `a9e5ac3` and **kept unmodified**.
  It is a pinned copy, not a patched fork, so the project builds against exactly the driver it was developed with.
- **How it is installed**: `install.sh` builds it against the running kernel and installs it like this (you can do
  the same by hand):

  ```sh
  make -C vendor/uniwill-laptop
  sudo mkdir -p /usr/lib/modules/$(uname -r)/updates/
  sudo cp vendor/uniwill-laptop/uniwill-laptop.ko /usr/lib/modules/$(uname -r)/updates/
  sudo depmod -a
  sudo modprobe uniwill_laptop
  ```

- `updates/` takes precedence over the kernel's own copy of the driver.
- **Rebuild it after every kernel update**: rerun `./install.sh` (there is no DKMS integration).
- `force=1` (in `/etc/modprobe.d/ripple-nova.conf`, and in the service) is needed because this laptop's DMI is the
  generic "Standard", which is not on the driver's list.
- If a different build is already loaded, the installer says so; reboot, or
  `sudo systemctl stop ripple-nova && sudo modprobe -r uniwill_laptop && sudo modprobe uniwill_laptop && sudo systemctl start ripple-nova`.
- **Update the copy**:
  `git subtree pull --prefix=vendor/uniwill-laptop https://github.com/Wer-Wolf/uniwill-laptop master --squash`.
- **License**: the driver is GPL-2.0 (`vendor/uniwill-laptop/LICENSE`); the rest of the project is [MIT or Apache-2.0](#license).

## The NVIDIA driver

Only needed for the GPU power settings. The EC holds the GPU's configurable TGP offset (cTGP) and the Dynamic Boost
values; the kernel driver and `rcc` write them whatever GPU driver you run. The GPU obeys them through NVIDIA's
driver (as far as we can tell it finds them through the `NVDA0820` firmware device; the kernel driver's documentation
calls cTGP an offset "on top of the base TGP" and does not say who applies it). Nouveau does not read them and registers no GPU power or temperature sensor, so
under nouveau the cTGP value is stored and you cannot see any effect. Nothing else in this project needs the NVIDIA
driver; the window, the profiles, the fans and the keyboard work the same with nouveau.

Measured on the XMG laptop this was developed on (RTX 3050 Ti Mobile, driver 620.70.06): `nvidia-smi` shows a 60 W
power limit at cTGP offset 0 and 70 W at `rcc system set ctgp-offset 10` (maximum 95 W), within a few seconds and
back at 0. **Dynamic Boost is not confirmed** there: the firmware does not answer NVIDIA's requests for a target
temperature and a platform power mode (`NVRM ... failed to get target temp from SBIOS` in `journalctl -k`), and
`nvidia-powerd` logs that the firmware asked it to disable the Dynamic Boost DC controller. Idle, the GPU sits at
about 5 W (P8) because runtime D3 power management is off by default in the driver.

Arch, with the stock `linux` kernel (Turing and newer GPUs; for another kernel use `nvidia-open-dkms`):

```sh
sudo pacman -S --needed nvidia-open nvidia-utils nvidia-prime
sudo systemctl enable nvidia-powerd.service     # Dynamic Boost; it needs Ampere or newer and firmware support
sudo reboot
```

- `nvidia-utils` blacklists nouveau (`/usr/lib/modprobe.d/nvidia-utils.conf`). With the `kms` mkinitcpio hook the
  boot image has to be rebuilt to carry that blacklist; the install does it by itself (the modules land in
  `extramodules`). Check: `sudo lsinitcpio /boot/EFI/Linux/arch-linux.efi | grep nvidia-utils.conf`.
- **Secure Boot with a unified kernel image signed by `sbctl`**: sbctl's mkinitcpio hook signs the rebuilt image
  (`sudo sbctl verify` lists nothing unsigned). Before installing, keep a signed copy of the working image: systemd-boot
  then offers it as a second entry (press a key in the 20 s menu) if the new one does not come up:
  `sudo cp -p /boot/EFI/Linux/arch-linux.efi /boot/EFI/Linux/arch-linux-nouveau.efi`.
- An external monitor on the laptop's HDMI/DisplayPort is wired to the NVIDIA GPU: it is the first thing that goes
  dark if the driver does not load. The internal panel is on the Intel GPU.
- `modeset` is on by default in the packaged driver (`modinfo -p nvidia_drm`).

After the reboot:

```sh
lsmod | grep -E '^nvidia|^nouveau'               # nvidia modules, no nouveau
nvidia-smi                                       # the GPU and the driver version
nvidia-smi -q -d POWER                           # the power limits: do they move with cTGP?
cat /proc/driver/nvidia/gpus/*/power             # the firmware's Dynamic Boost support (NVIDIA's README)
systemctl status nvidia-powerd                   # stops by itself after a few failed starts when the firmware says no
sudo rcc system set ctgp-offset 10               # then `nvidia-smi -q -d POWER` again; 0 puts it back
prime-run glxinfo -B                             # run one program on the NVIDIA GPU (nvidia-prime; needs mesa-utils)
```

Back to nouveau:

```sh
sudo systemctl disable nvidia-powerd.service
sudo pacman -Rns nvidia-open nvidia-utils nvidia-prime
sudo mkinitcpio -P                               # drops the blacklist from the boot image (sbctl signs it again)
sudo reboot
```

## Passwordless sudo

The window needs root (EC access), and the stock answer is a polkit password dialog every time it starts. So
`install.sh` writes `/etc/sudoers.d/ripple-nova` (validated with `visudo` first, mode 0440) that lets **your user
start the window as root without a password**. It grants exactly:

- `/usr/local/bin/rcc-gui` with no arguments, and the root-owned copy of it that *Open at login* installs
  (`/usr/local/lib/ripple-nova/rcc-gui`, also with no arguments),
- `rcc touchpad on`, `off` and `toggle`, `rcc mode set office`, `gaming` and `turbo` (the tray icon's
  profile shortcut), and `rcc popups on` and `off` (its popups switch): these exact commands, no other arguments,
  no `custom`,
- with the four display variables (`WAYLAND_DISPLAY XDG_RUNTIME_DIR DISPLAY XAUTHORITY`) kept for the window
  command only, and **no `SETENV`** (that would allow `LD_PRELOAD` and amount to full root).

What that means: any process running as you can start the window as root and drive it (EC settings, the files in
`/etc/ripple-nova`, the login entry), and switch the profile, the touchpad or the popups, but cannot run anything else as root through it. It is a
convenience with a real, bounded cost: if you do not want it, use `./install.sh --no-sudoers`, or remove it later
with `sudo rm /etc/sudoers.d/ripple-nova`. The launcher then falls back to polkit.

The daemon is a separate root systemd service; it needs no rule.

## Custom fan profile

Switch it on in the Fan tab (or `rcc fan custom on`). The daemon drives both fans from the two curves of the profile
that is running (each profile keeps its own; the default is the Windows app's for its mode, read off its Fan Settings
page: its points are drawn evenly spaced, so they sit here at 5 °C steps from 40 to 85 °C). Anything else about the
machine (profile, lights) is unchanged. Safety rules, all enforced in code:

- at 90 °C on either sensor, or with no readable sensor, **both fans run at 100 %**;
- a curve must reach at least 60 % at 85 °C and may never fall as the temperature rises;
- fans slow down gradually (2 points per second), speed up at once;
- turning it off, stopping the service or a service crash hands the fans back to the firmware
  (`ExecStopPost=rcc fan release`). If you stop a manual `rcc fan custom run` with Ctrl+C, run `rcc fan release`.

Use the daemon's engine only: never run `rcc fan custom run` next to the service.

## Command line

```sh
sudo rcc doctor                      # what works on this machine, and how to fix what does not
sudo rcc status                      # profile, limits, temperatures, fans, battery
sudo rcc mode get | set office|gaming|turbo|"<profile name>" | next | list
sudo rcc mode custom "Profile 1" 45 50 --pl4 120 --ctgp 5  # make or change one of your profiles; reset-custom <name> / delete <name>
sudo rcc fan boost on|off            # refused while the custom fan profile is on
sudo rcc fan custom show|on|off|reset|set --cpu 30,35,48,55,85 --gpu 0,35,48,55,85   # --profile <office|gaming|turbo|name>, default: the running one
sudo rcc rgb color ff8800 | rainbow on|off | backlight on|off | brightness 60
sudo rcc touchpad on|off|toggle|status
sudo rcc popups [on|off]
sudo rcc system                      # kernel toggles; `rcc system set fn-lock on`
sudo rcc battery get | profile performance|balanced|healthy   # the battery charge mode: Standard|Long_Life|Trickle
sudo rcc gpu                         # NVIDIA GPU: power limit, clocks, offsets (needs the NVIDIA driver)
sudo rcc gpu set --core 50 --mem 200 # save the clock offsets in MHz; `rcc gpu on` applies them, `rcc gpu off` undoes it
```

## Risks

- This writes EC registers and drives fans by hand. The failsafes above exist, but a wrong register can cost you
  a reboot; the EC rewrites some of what we write, and the register map came from reverse engineering.
- Raw EC writes (`rcc ec write`) are refused outside the known register map unless you add `--force`.
- Turbo applies 65 W PL1/PL2 and 145 W PL4 (the EC's own default for that mode, what the Windows app calls Overboost) and runs hot.
- The register map in `crates/ripple-core/src/regs.rs` was extracted from the OEM service. The OEM packages
  themselves are proprietary and are not part of this repository (`ControlCenter_*` is git-ignored).
- Not affiliated with XMG, Uniwill or Tongfang.

## Development

```sh
cargo build --workspace && cargo test --workspace && cargo clippy --workspace
cargo fmt --all
sudo ./target/debug/rcc doctor       # run the debug build as root (there is no mock EC any more)
```

Layout: `crates/ripple-core` (register map, protocol, parsers), `crates/ripple-hw` (EC backends; `control/` has
the profiles, config, keyboard, fans, platform toggles and the daemon loops), `crates/ripple-cli` (`rcc`),
`crates/ripple-gui` (`rcc-gui`; `tabs/` has one file per tab), `crates/ripple-tray` (`rcc-tray`), `dist/` (service,
launcher, sudoers, icon, desktop entries), `vendor/` (the `uniwill-laptop` subtree).

Files that several processes edit (`/etc/ripple-nova/config.json`, `gui.json`) are only changed through
`Config::update` / `store::set_json_keys`: one locked read-modify-write, replaced atomically. Do not load, change and
save a whole copy. CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, clippy with warnings denied, and the tests.

`Cargo.lock` keeps `libc` at 0.2.189 on purpose: GPUI pulls in `xattr` 0.2.3, which needs `libc::ENOATTR`, and libc
0.2.190 removed it on Linux. Do not run `cargo update -p libc` until GPUI moves to a newer `xattr`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

The vendored kernel driver `vendor/uniwill-laptop` is not covered by that: it keeps its own licence, GPL-2.0
(`vendor/uniwill-laptop/LICENSE`).

## Thanks

To [Wer-Wolf/uniwill-laptop](https://github.com/Wer-Wolf/uniwill-laptop), which is built on
[qc71_laptop](https://github.com/pobrn/qc71_laptop) and [tuxedo-drivers](https://github.com/tuxedocomputers/tuxedo-drivers),
for the driver this project stands on.
