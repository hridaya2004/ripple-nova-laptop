//! The Graphics tab (only with NVIDIA's driver): the GPU's state and its clock offsets.

use crate::*;
use ripple_hw::gpu::{CORE_RANGE_MHZ, MEM_RANGE_MHZ};

/// The offsets a slider can take: `range` in steps of `by` MHz.
fn steps(range: (i32, i32), by: usize) -> Vec<i32> {
    (range.0..=range.1).step_by(by).collect()
}

/// The slider position closest to `value`.
fn nearest(steps: &[i32], value: i32) -> usize {
    steps
        .iter()
        .enumerate()
        .min_by_key(|(_, s)| s.abs_diff(value))
        .map_or(0, |(i, _)| i)
}

impl Ui {
    pub(crate) fn graphics_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let Some(g) = &self.snap.gpu else {
            return section(p, "Graphics")
                .child(note(p, "The NVIDIA driver is not loaded."))
                .into_any_element();
        };
        let (s, t) = (&g.status, g.tune);
        let w = |v: Option<f64>| v.map_or("n/a".to_string(), |v| format!("{v:.0} W"));
        let mhz = |v: Option<u32>| v.map_or("n/a".to_string(), |v| format!("{v} MHz"));

        let state = section(p, &s.name)
            .child(row(p, "Power", w(s.power_w)))
            .child(row(
                p,
                "Power limit",
                format!(
                    "{} (default {}, at most {})",
                    w(s.limit_w),
                    w(s.default_limit_w),
                    w(s.max_limit_w)
                ),
            ))
            .child(row(
                p,
                "Temperature",
                s.temp_c.map_or("n/a".to_string(), |c| format!("{c} °C")),
            ))
            .child(row(p, "Core clock", mhz(s.core_mhz)))
            .child(row(p, "Memory clock", mhz(s.mem_mhz)))
            .child(row(
                p,
                "Performance state",
                s.pstate.map_or("n/a".to_string(), |v| format!("P{v}")),
            ))
            .child(note(
                p,
                "The power limit is the GPU's base power plus the cTGP of the running profile (Profiles tab).",
            ));

        let core_steps = steps(CORE_RANGE_MHZ, 10);
        let mem_steps = steps(MEM_RANGE_MHZ, 50);
        let core_at = nearest(&core_steps, t.core_mhz);
        let mem_at = nearest(&mem_steps, t.mem_mhz);
        let core_labels = vec![
            (0, format!("{}", CORE_RANGE_MHZ.0)),
            (core_steps.len() - 1, format!("{}", CORE_RANGE_MHZ.1)),
        ];
        let mem_labels = vec![
            (0, format!("{}", MEM_RANGE_MHZ.0)),
            (mem_steps.len() - 1, format!("{}", MEM_RANGE_MHZ.1)),
        ];
        let core = self.slider(
            cx,
            SliderSpec {
                id: "gpu-core",
                n: core_steps.len(),
                value: core_at,
                fill: false,
                labels: core_labels,
                cells: None,
            },
            move |i| {
                let v = core_steps[i];
                Box::new(move |hw| hw.set_gpu_offsets(Some(v), None))
            },
        );
        let mem = self.slider(
            cx,
            SliderSpec {
                id: "gpu-mem",
                n: mem_steps.len(),
                value: mem_at,
                fill: false,
                labels: mem_labels,
                cells: None,
            },
            move |i| {
                let v = mem_steps[i];
                Box::new(move |hw| hw.set_gpu_offsets(None, Some(v)))
            },
        );
        let offsets = section(p, "Clock offsets")
            .child(self.switch_row(
                cx,
                "gpu-apply",
                "Apply the offsets",
                "Off: the GPU keeps its own clocks. The service puts the offsets back after a driver reload.",
                t.enabled,
                move |ui, cx| {
                    let on = !t.enabled;
                    ui.act(cx, move |hw| hw.set_gpu_enabled(on));
                },
            ))
            .child(row(p, "Core clock offset", format!("{:+} MHz", t.core_mhz)))
            .child(core)
            .child(row(
                p,
                "Memory clock offset",
                format!("{:+} MHz", t.mem_mhz),
            ))
            .child(mem)
            .child(div().flex().child(
                button(p, "gpu-reset", "Reset to 0", false).on_click(cx.listener(
                    |ui, _, _, cx| ui.act(cx, |hw| hw.set_gpu_offsets(Some(0), Some(0))),
                )),
            ))
            .child(note(
                p,
                "Too high an offset can freeze the screen or crash the GPU until you reboot: start small (+50 core, +200 memory) and test under load.",
            ));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(state)
            .child(offsets)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_positions_cover_the_range_and_pick_the_nearest_step() {
        let core = steps(CORE_RANGE_MHZ, 10);
        assert_eq!(
            (core[0], *core.last().unwrap(), core.len()),
            (-200, 200, 41)
        );
        assert_eq!(core[nearest(&core, 0)], 0);
        assert_eq!(core[nearest(&core, 14)], 10);
        assert_eq!(core[nearest(&core, 16)], 20);
        assert_eq!(
            core[nearest(&core, 999)],
            200,
            "saved values past the end clamp"
        );
        let mem = steps(MEM_RANGE_MHZ, 50);
        assert_eq!((mem[0], *mem.last().unwrap()), (-500, 1000));
        assert_eq!(nearest(&[], 5), 0);
    }
}
