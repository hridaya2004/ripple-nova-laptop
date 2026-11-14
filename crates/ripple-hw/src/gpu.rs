//! The NVIDIA GPU through NVML (`libnvidia-ml.so.1`). The library only exists with NVIDIA's own driver,
//! so it is loaded at run time: without the driver nothing here is available and nothing links against it.
//! What it does here: read the GPU's state, and set the core and memory clock offsets (what the OEM app's
//! "GPU Core / Memory Clock Offset" sliders do). Every NVML call that changes something needs root.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{HwError, HwResult};

/// The offsets this app allows, in MHz either way. NVML itself allows much more on this GPU (core
/// -1000..=1000, memory -2000..=6000); these keep a slider from reaching a setting that hangs the machine.
pub const CORE_RANGE_MHZ: (i32, i32) = (-200, 200);
pub const MEM_RANGE_MHZ: (i32, i32) = (-500, 1000);

/// The clock offsets the user chose. `enabled` is the safety switch: off, the GPU keeps its own clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GpuTune {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub core_mhz: i32,
    #[serde(default)]
    pub mem_mhz: i32,
}

impl GpuTune {
    pub fn validate(&self) -> Result<(), String> {
        if !(CORE_RANGE_MHZ.0..=CORE_RANGE_MHZ.1).contains(&self.core_mhz) {
            return Err(format!(
                "GPU core clock offset must be {}..={} MHz (got {})",
                CORE_RANGE_MHZ.0, CORE_RANGE_MHZ.1, self.core_mhz
            ));
        }
        if !(MEM_RANGE_MHZ.0..=MEM_RANGE_MHZ.1).contains(&self.mem_mhz) {
            return Err(format!(
                "GPU memory clock offset must be {}..={} MHz (got {})",
                MEM_RANGE_MHZ.0, MEM_RANGE_MHZ.1, self.mem_mhz
            ));
        }
        Ok(())
    }

    /// The (core, memory) offsets the GPU should have: the chosen ones when on, else none.
    pub fn wanted(&self) -> (i32, i32) {
        if self.enabled {
            (self.core_mhz, self.mem_mhz)
        } else {
            (0, 0)
        }
    }
}

/// Is NVIDIA's kernel driver loaded? (`/proc/driver/nvidia` exists only then; nouveau has no such file.)
pub fn driver_present() -> bool {
    driver_present_at(Path::new("/proc/driver/nvidia/version"))
}

fn driver_present_at(version_file: &Path) -> bool {
    version_file.is_file()
}

type Dev = *mut c_void;
type GetInt = unsafe extern "C" fn(Dev, *mut c_int) -> c_int;
type SetInt = unsafe extern "C" fn(Dev, c_int) -> c_int;
type GetUint = unsafe extern "C" fn(Dev, *mut c_uint) -> c_int;
type GetUintOf = unsafe extern "C" fn(Dev, c_int, *mut c_uint) -> c_int;
type MinMax = unsafe extern "C" fn(Dev, *mut c_int, *mut c_int) -> c_int;
type Limits = unsafe extern "C" fn(Dev, *mut c_uint, *mut c_uint) -> c_int;

/// An open NVML session on the first GPU.
pub struct Gpu {
    dev: Dev,
    name: String,
    get_core: GetInt,
    set_core: SetInt,
    get_mem: GetInt,
    set_mem: SetInt,
    range_core: Option<MinMax>,
    range_mem: Option<MinMax>,
    limit: GetUint,
    default_limit: GetUint,
    limits: Limits,
    usage: GetUint,
    temp: GetUintOf,
    clock: GetUintOf,
    pstate: GetInt,
}

// SAFETY: NVML is documented as thread safe, and `dev` is only an opaque handle it gave us.
unsafe impl Send for Gpu {}

/// What the window shows about the GPU. Watts are NVML's milliwatts divided down.
#[derive(Debug, Clone, Default)]
pub struct GpuStatus {
    pub name: String,
    pub power_w: Option<f64>,
    /// The power limit in force now (base TGP plus cTGP), its default, and the most NVML allows.
    pub limit_w: Option<f64>,
    pub default_limit_w: Option<f64>,
    pub max_limit_w: Option<f64>,
    pub temp_c: Option<u32>,
    pub core_mhz: Option<u32>,
    pub mem_mhz: Option<u32>,
    pub pstate: Option<i32>,
    /// The offsets the GPU has now.
    pub core_offset: i32,
    pub mem_offset: i32,
}

fn describe(code: c_int) -> &'static str {
    match code {
        1 => "NVML not initialized",
        2 => "invalid argument",
        3 => "not supported by this GPU",
        4 => "no permission (needs root)",
        _ => "error",
    }
}

fn check(call: &str, code: c_int) -> HwResult<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(HwError::Other(format!(
            "NVML {call}: {} ({code})",
            describe(code)
        )))
    }
}

/// Look `name` up in the loaded library as a function of type `F`.
///
/// # Safety
/// `F` must be the real signature of the NVML function called `name`.
unsafe fn sym<F: Copy>(lib: *mut c_void, name: &CStr) -> Option<F> {
    // SAFETY: dlsym on a handle dlopen returned, with a valid C string.
    let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
    // SAFETY: the caller promises F is the function's type; a function pointer is pointer-sized.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) })
}

fn missing(name: &str) -> HwError {
    HwError::Unavailable(format!("libnvidia-ml.so.1 has no {name}"))
}

fn read<T: Default>(f: unsafe extern "C" fn(Dev, *mut T) -> c_int, dev: Dev) -> Option<T> {
    let mut v = T::default();
    // SAFETY: `f` is an NVML getter taking a device handle and one out-pointer of type T.
    (unsafe { f(dev, &mut v) } == 0).then_some(v)
}

fn read_of(f: GetUintOf, dev: Dev, which: c_int) -> Option<u32> {
    let mut v = 0;
    // SAFETY: `f` is an NVML getter taking a device handle, a selector and one out-pointer.
    (unsafe { f(dev, which, &mut v) } == 0).then_some(v)
}

impl Gpu {
    /// Load NVML and open the first GPU. `Unavailable` when the NVIDIA driver is not there.
    pub fn open() -> HwResult<Self> {
        if !driver_present() {
            return Err(HwError::Unavailable(
                "the NVIDIA driver is not loaded".into(),
            ));
        }
        // SAFETY: dlopen with a valid C string; the handle is kept for the life of the process.
        let lib = unsafe { libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            return Err(HwError::Unavailable(
                "libnvidia-ml.so.1 not found (package nvidia-utils)".into(),
            ));
        }
        // each type below is the signature NVML documents for that function
        unsafe {
            let init: unsafe extern "C" fn() -> c_int =
                sym(lib, c"nvmlInit_v2").ok_or_else(|| missing("nvmlInit_v2"))?;
            check("nvmlInit_v2", init())?;
            let by_index: unsafe extern "C" fn(c_uint, *mut Dev) -> c_int =
                sym(lib, c"nvmlDeviceGetHandleByIndex_v2")
                    .ok_or_else(|| missing("nvmlDeviceGetHandleByIndex_v2"))?;
            let mut dev: Dev = std::ptr::null_mut();
            check("nvmlDeviceGetHandleByIndex_v2", by_index(0, &mut dev))?;
            let name_fn: unsafe extern "C" fn(Dev, *mut c_char, c_uint) -> c_int =
                sym(lib, c"nvmlDeviceGetName").ok_or_else(|| missing("nvmlDeviceGetName"))?;
            let mut buf = [0 as c_char; 96];
            check("nvmlDeviceGetName", name_fn(dev, buf.as_mut_ptr(), 96))?;
            let name = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
            macro_rules! need {
                ($n:literal) => {
                    sym(lib, $n).ok_or_else(|| missing(stringify!($n)))?
                };
            }
            Ok(Self {
                dev,
                name,
                get_core: need!(c"nvmlDeviceGetGpcClkVfOffset"),
                set_core: need!(c"nvmlDeviceSetGpcClkVfOffset"),
                get_mem: need!(c"nvmlDeviceGetMemClkVfOffset"),
                set_mem: need!(c"nvmlDeviceSetMemClkVfOffset"),
                range_core: sym(lib, c"nvmlDeviceGetGpcClkMinMaxVfOffset"),
                range_mem: sym(lib, c"nvmlDeviceGetMemClkMinMaxVfOffset"),
                limit: need!(c"nvmlDeviceGetEnforcedPowerLimit"),
                default_limit: need!(c"nvmlDeviceGetPowerManagementDefaultLimit"),
                limits: need!(c"nvmlDeviceGetPowerManagementLimitConstraints"),
                usage: need!(c"nvmlDeviceGetPowerUsage"),
                temp: need!(c"nvmlDeviceGetTemperature"),
                clock: need!(c"nvmlDeviceGetClockInfo"),
                pstate: need!(c"nvmlDeviceGetPerformanceState"),
            })
        }
    }

    /// The (core, memory) offsets the GPU has now, in MHz.
    pub fn offsets(&self) -> HwResult<(i32, i32)> {
        let core = read(self.get_core, self.dev)
            .ok_or_else(|| HwError::Other("NVML: cannot read the core clock offset".into()))?;
        let mem = read(self.get_mem, self.dev)
            .ok_or_else(|| HwError::Other("NVML: cannot read the memory clock offset".into()))?;
        Ok((core, mem))
    }

    /// Set both offsets (MHz). They are checked against this app's ranges and against what the GPU allows.
    pub fn set_offsets(&self, core: i32, mem: i32) -> HwResult<()> {
        GpuTune {
            enabled: true,
            core_mhz: core,
            mem_mhz: mem,
        }
        .validate()?;
        for (what, range, v) in [
            ("core", self.range_core, core),
            ("memory", self.range_mem, mem),
        ] {
            let Some(f) = range else { continue };
            let (mut lo, mut hi) = (0, 0);
            // SAFETY: an NVML getter with a device handle and two out-pointers.
            if unsafe { f(self.dev, &mut lo, &mut hi) } == 0 && !(lo..=hi).contains(&v) {
                return Err(HwError::Other(format!(
                    "this GPU allows {what} clock offsets {lo}..={hi} MHz (got {v})"
                )));
            }
        }
        // SAFETY: NVML setters taking a device handle and the offset.
        check("nvmlDeviceSetGpcClkVfOffset", unsafe {
            (self.set_core)(self.dev, core)
        })?;
        check("nvmlDeviceSetMemClkVfOffset", unsafe {
            (self.set_mem)(self.dev, mem)
        })
    }

    /// Bring the GPU's offsets to what `tune` wants. `true` when something was set.
    pub fn apply(&self, tune: &GpuTune) -> HwResult<bool> {
        let (core, mem) = tune.wanted();
        if self.offsets()? == (core, mem) {
            return Ok(false);
        }
        self.set_offsets(core, mem)?;
        Ok(true)
    }

    pub fn status(&self) -> GpuStatus {
        let w = |mw: u32| f64::from(mw) / 1000.0;
        let (mut lo, mut hi) = (0, 0);
        // SAFETY: an NVML getter with a device handle and two out-pointers.
        let max = (unsafe { (self.limits)(self.dev, &mut lo, &mut hi) } == 0).then_some(w(hi));
        let (core_offset, mem_offset) = self.offsets().unwrap_or((0, 0));
        GpuStatus {
            name: self.name.clone(),
            power_w: read(self.usage, self.dev).map(w),
            limit_w: read(self.limit, self.dev).map(w),
            default_limit_w: read(self.default_limit, self.dev).map(w),
            max_limit_w: max,
            temp_c: read_of(self.temp, self.dev, 0),
            core_mhz: read_of(self.clock, self.dev, 0),
            mem_mhz: read_of(self.clock, self.dev, 2),
            pstate: read(self.pstate, self.dev),
            core_offset,
            mem_offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_stay_inside_the_ranges_and_off_means_none() {
        let t = |core, mem| GpuTune {
            enabled: true,
            core_mhz: core,
            mem_mhz: mem,
        };
        assert!(t(0, 0).validate().is_ok());
        assert!(t(200, 1000).validate().is_ok());
        assert!(t(-200, -500).validate().is_ok());
        assert!(t(201, 0).validate().is_err());
        assert!(t(0, 1001).validate().is_err());
        assert!(t(-201, 0).validate().is_err());
        assert_eq!(t(50, 100).wanted(), (50, 100));
        assert_eq!(
            GpuTune {
                enabled: false,
                ..t(50, 100)
            }
            .wanted(),
            (0, 0)
        );
        // an old config with no gpu keys is off with no offsets
        assert_eq!(
            serde_json::from_str::<GpuTune>("{}").unwrap(),
            GpuTune::default()
        );
    }

    #[test]
    fn the_driver_is_there_when_its_proc_file_is() {
        let dir = std::env::temp_dir().join(format!("ripple-gpu-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("version");
        assert!(!driver_present_at(&f));
        std::fs::write(&f, "NVRM version").unwrap();
        assert!(driver_present_at(&f));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nvml_errors_say_what_went_wrong() {
        assert!(check("x", 0).is_ok());
        let e = check("nvmlDeviceSetGpcClkVfOffset", 4)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("needs root") && e.contains("nvmlDeviceSetGpcClkVfOffset"),
            "{e}"
        );
    }
}
