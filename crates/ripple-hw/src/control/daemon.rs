//! What the daemon runs: popups, the custom fan loop, and the mode button.

use super::*;

/// `KEY_F14`: what the kernel's `uniwill-wmi` emits for the mode button.
pub const MODE_KEY_CODE: u16 = 184;

/// What the popups look at right now.
pub fn observed(
    ec: &mut dyn EcAccess,
    sysfs: Option<&SysfsEc>,
    stock: &[Preset; 3],
    cfg: &Config,
) -> crate::notify::Observed {
    crate::notify::Observed {
        profile: running(ec, stock, &cfg.profiles).ok().flatten(),
        pl: ec
            .read(addr::PL1_SETTING)
            .ok()
            .zip(ec.read(addr::PL2_SETTING).ok()),
        boosted: !cfg.fan.enabled && boost_is_on(ec),
        toggles: sysfs
            .map(|u| platform_toggles(u, has_lightbar(ec)))
            .unwrap_or_default(),
    }
}

/// Where the daemon publishes the profile it sees, as a lowercase name (`office`, `gaming`, `turbo`,
/// `custom`, `unknown`), rewritten every second. World-readable: the tray icon runs as the desktop user, cannot
/// read the EC, and treats a file older than a few seconds as "the daemon is not running".
pub const PROFILE_FILE: &str = "/run/ripple-nova-profile";

pub(super) fn publish_profile(path: &std::path::Path, p: Option<Profile>) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = path.with_extension("tmp");
    // written aside and renamed, so a reader never sees half a file
    let _ = std::fs::write(
        &tmp,
        p.map_or("unknown".into(), |p| p.name().to_lowercase()),
    )
    .and_then(|()| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)))
    .and_then(|()| std::fs::rename(&tmp, path));
}

/// The popup loop (the daemon runs it in a thread, so popups work with no window open): once a second,
/// look at the profile, fan boost and toggles, and pop up what changed when "Show popup" is on. The profile
/// is also published for the tray icon (`PROFILE_FILE`).
pub fn popup_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut ec = match crate::linux::open_auto() {
        Ok(ec) => ec,
        Err(e) => return eprintln!("popups: {e}"),
    };
    let sysfs = SysfsEc::probe().ok();
    let stock = stock(Some(ec.as_mut()));
    let mut notifier = crate::notify::Notifier::default();
    while !stop.load(Relaxed) {
        let cfg = Config::load();
        let seen = observed(ec.as_mut(), sysfs.as_ref(), &stock, &cfg);
        publish_profile(std::path::Path::new(PROFILE_FILE), seen.profile);
        let changes = notifier.observe(&seen);
        if !changes.is_empty() && crate::notify::popups_enabled() {
            for (title, body) in changes {
                crate::notify::send_to_logged_in(&title, &body);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// The engine's main loop (the daemon runs it in a thread): every tick, read the saved profile and
/// the sensors and drive the fans. Without the uniwill hwmon driver there are no sensors to go by,
/// so the fans stay with the firmware. Hands the fans back when `stop` is set.
pub fn fan_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut ec = match crate::linux::open_auto() {
        Ok(ec) => ec,
        Err(e) => return eprintln!("fan engine: {e}"),
    };
    let Ok(sysfs) = SysfsEc::probe() else {
        return eprintln!(
            "fan engine: no uniwill hwmon (kernel driver not loaded), fans stay with the firmware"
        );
    };
    let mut engine = FanEngine::default();
    let stock = stock(Some(ec.as_mut()));
    let mut last_err = String::new(); // a persistent error is reported once, not every tick
    while !stop.load(Relaxed) {
        // each profile has its own curves: the ones of what is running (Gaming's when it matches none)
        let cfg = Config::load();
        let now = running(ec.as_mut(), &stock, &cfg.profiles).ok().flatten();
        let curves = cfg.fan_curves(now.unwrap_or(Profile::Stock(OperatingMode::Gaming)));
        let (cpu, gpu) = sysfs.sensors().map_or((None, None), |s| cpu_gpu_temps(&s));
        match engine.tick(ec.as_mut(), &curves, cpu, gpu) {
            Ok(()) => last_err.clear(),
            Err(e) if e.to_string() != last_err => {
                last_err = e.to_string();
                eprintln!("fan engine: {last_err}");
            }
            Err(_) => {}
        }
        std::thread::sleep(FAN_TICK);
    }
    let _ = engine.stop(ec.as_mut());
}

/// Only one process should follow the mode button (two would each cycle the profile). Followers
/// hold this lock for as long as they follow: the daemon (`rcc watch`) waits for it, the GUI only
/// tries, and stays out of the way when the daemon already holds it.
const FOLLOW_LOCK: &str = "/run/ripple-nova-button.lock";

pub fn follow_lock(blocking: bool) -> Option<std::fs::File> {
    follow_lock_at(std::path::Path::new(FOLLOW_LOCK), blocking)
}

pub(super) fn follow_lock_at(path: &std::path::Path, blocking: bool) -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    if blocking {
        f.lock().ok()?;
    } else {
        f.try_lock().ok()?;
    }
    Some(f)
}

/// Open the input device called `name` (root only).
fn open_input_named(name: &str) -> HwResult<std::fs::File> {
    for e in std::fs::read_dir("/sys/class/input")?.flatten() {
        let node = e.file_name().to_string_lossy().to_string();
        if !node.starts_with("event") {
            continue;
        }
        let dev_name = std::fs::read_to_string(e.path().join("device/name")).unwrap_or_default();
        if dev_name.trim() == name {
            return Ok(std::fs::File::open(format!("/dev/input/{node}"))?);
        }
    }
    Err(HwError::Unavailable(format!("no '{name}' input device")))
}

/// Open the "Uniwill WMI hotkeys" input device (root only).
pub fn open_mode_key() -> HwResult<std::fs::File> {
    open_input_named("Uniwill WMI hotkeys").map_err(|_| {
        HwError::Unavailable(
            "no 'Uniwill WMI hotkeys' input device (uniwill-laptop not loaded?)".into(),
        )
    })
}

/// The next input event of a device as `(type, code, value)`; blocks.
fn read_input_event(dev: &mut std::fs::File) -> std::io::Result<(u16, u16, i32)> {
    // struct input_event { struct timeval (2 words); u16 type; u16 code; i32 value }
    let tv = 2 * std::mem::size_of::<usize>();
    let mut buf = vec![0u8; tv + 8];
    dev.read_exact(&mut buf)?;
    Ok((
        u16::from_le_bytes([buf[tv], buf[tv + 1]]),
        u16::from_le_bytes([buf[tv + 2], buf[tv + 3]]),
        i32::from_le_bytes([buf[tv + 4], buf[tv + 5], buf[tv + 6], buf[tv + 7]]),
    ))
}

/// Block until the mode button is pressed once (key 184 down).
pub fn wait_mode_key(dev: &mut std::fs::File) -> HwResult<()> {
    loop {
        let (typ, code, value) = read_input_event(dev)?;
        if typ == 1 && code == MODE_KEY_CODE && value == 1 {
            return Ok(());
        }
    }
}

/// The daemon's touchpad-gesture loop. The corner double tap reaches the system only as keys the embedded
/// controller types on the internal keyboard (Left Ctrl + Left Meta + F24); the kernel does nothing with them
/// and GNOME did not act on the real ones, so the daemon does what the firmware's own gesture does: it switches
/// the touchpad's surface off or on (LED included). The pad stays awake, so the same gesture brings it back.
pub fn touchpad_key_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    // never start with the pad silenced by an earlier run
    if crate::touchpad::surface_on() == Some(false) {
        let _ = crate::touchpad::set_surface(true);
    }
    let mut said = String::new(); // a standing problem is logged once
    while !stop.load(Relaxed) {
        let mut keys = match open_input_named("AT Translated Set 2 keyboard") {
            Ok(k) => k,
            Err(e) => {
                if said != e.to_string() {
                    said = e.to_string();
                    eprintln!("touchpad gesture: {e}");
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
                continue;
            }
        };
        let mut chord = crate::touchpad::ToggleKey::default();
        while let Ok((typ, code, value)) = read_input_event(&mut keys) {
            if typ != 1 || !chord.feed(code, value) {
                continue;
            }
            match crate::touchpad::toggle_surface() {
                Ok(on) => {
                    if crate::notify::popups_enabled() {
                        crate::notify::send_to_logged_in("Touchpad", if on { "on" } else { "off" });
                    }
                }
                Err(e) => eprintln!("touchpad gesture: {e}"),
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2)); // the device went away: look for it again
    }
}

/// The daemon's GPU loop (NVIDIA driver only): every five seconds, make the GPU's clock offsets match the
/// saved choice. They are lost when the driver reloads, so they are put back here. Switching them off sets
/// zeros once; a GPU nobody here tuned is left alone, so another tool's offsets are not fought.
pub fn gpu_loop(stop: &std::sync::atomic::AtomicBool) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut gpu: Option<crate::gpu::Gpu> = None;
    let mut applied = false; // did we set offsets (so that "off" has to put zeros back)
    let mut said = String::new(); // the last complaint, so a standing problem is logged once
    let mut complain = |msg: String| {
        if msg != said {
            eprintln!("gpu: {msg}");
            said = msg;
        }
    };
    while !stop.load(Relaxed) {
        if crate::gpu::driver_present() {
            if gpu.is_none() {
                gpu = crate::gpu::Gpu::open()
                    .map_err(|e| complain(e.to_string()))
                    .ok();
            }
            let tune = Config::load().gpu;
            if let Some(g) = gpu.as_ref().filter(|_| tune.enabled || applied) {
                match g.apply(&tune) {
                    Ok(_) => applied = tune.enabled,
                    Err(e) => complain(e.to_string()),
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}
