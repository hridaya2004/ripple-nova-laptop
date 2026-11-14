//! The Fan tab: mode, sensors, the curve charts, auto-boost.

use crate::*;

/// Paint one fan curve into `b`: grid, the area under the curve, the line and the five dots (the one
/// being dragged, if any, bigger).
pub(crate) fn paint_chart(
    window: &mut Window,
    b: Bounds<Pixels>,
    p: Palette,
    shown: &[u8; 5],
    active: Option<usize>,
    dim: bool,
) {
    // curves that cannot be edited are drawn in the muted colour
    let ink = if dim { p.muted } else { p.accent };
    let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let at = |temp: f32, duty: f32| {
        point(
            px(x0 + chart::frac_x(temp) * w),
            px(y0 + (1. - duty / 100.) * h),
        )
    };
    for k in 0..=10 {
        let y = y0 + h * (1. - k as f32 / 10.);
        window.paint_quad(fill(
            Bounds::new(point(px(x0), px(y - 0.5)), size(px(w), px(1.))),
            rgb(p.border),
        ));
    }
    for t in (30..=100).step_by(10) {
        let x = x0 + chart::frac_x(t as f32) * w;
        window.paint_quad(fill(
            Bounds::new(point(px(x - 0.5), px(y0)), size(px(1.), px(h))),
            rgb(p.border),
        ));
    }
    let pts = chart::outline(shown);
    let mut area = PathBuilder::fill();
    area.move_to(at(pts[0].0, 0.));
    for (t, d) in &pts {
        area.line_to(at(*t, *d));
    }
    area.line_to(at(chart::T_MAX, 0.));
    area.close();
    if let Ok(path) = area.build() {
        window.paint_path(path, rgba((ink << 8) | 0x33));
    }
    let mut line = PathBuilder::stroke(px(2.));
    line.move_to(at(pts[0].0, pts[0].1));
    for (t, d) in &pts[1..] {
        line.line_to(at(*t, *d));
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, rgb(ink));
    }
    for (i, d) in shown.iter().enumerate() {
        let c = at(f32::from(control::FAN_ANCHORS[i]), f32::from(*d));
        let r = px(if active == Some(i) { 7. } else { 5. });
        window.paint_quad(quad(
            Bounds::new(point(c.x - r, c.y - r), size(r * 2., r * 2.)),
            Corners::all(r),
            rgb(ink),
            Edges::all(px(2.)),
            rgb(p.panel),
            BorderStyle::Solid,
        ));
    }
}

impl Ui {
    pub(crate) fn fan_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;

        let current = if s.fan.enabled {
            FanMode::Custom
        } else if s.boosted {
            FanMode::Boost
        } else {
            FanMode::Auto
        };
        let mode_button =
            |cx: &mut Context<Self>, id: &'static str, label: &'static str, m: FanMode| {
                button(p, id, label, current == m).on_click(
                    cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_fan_mode(m))),
                )
            };
        let mode = section(p, "Fan mode")
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(mode_button(cx, "fan-auto", "Auto", FanMode::Auto))
                    .child(mode_button(cx, "fan-custom", "Custom", FanMode::Custom))
                    .child(mode_button(cx, "fan-boost", "Boost", FanMode::Boost)),
            )
            .child(note(p, "Auto: the firmware's own curve. Custom: your curves for the running profile below, any time. Boost: both fans at full speed."));

        // one tile per reading, wrapping when the window is narrow: what you watch while changing the mode
        let tile = |label: String, value: String| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(150.))
                .gap_1()
                .p_3()
                .rounded_md()
                .bg(rgb(p.bg))
                .child(
                    div()
                        .text_size(px(TEXT_CAPTION))
                        .text_color(rgb(p.muted))
                        .child(label),
                )
                .child(div().child(value))
        };
        let temps = s
            .temps
            .iter()
            .map(|(l, t)| tile(l.clone(), format!("{t:.1} °C")));
        let rpms = s.fans.iter().map(|(l, rpm, duty)| {
            tile(
                l.clone(),
                format!(
                    "{rpm} rpm{}",
                    duty.map(|d| format!("  ({d} %)")).unwrap_or_default()
                ),
            )
        });
        let mut fans = section(p, "Sensors");
        fans = if s.temps.is_empty() && s.fans.is_empty() {
            fans.child(note(p, "No hwmon sensors (kernel driver not loaded?)."))
        } else {
            fans.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(temps.chain(rpms).collect::<Vec<_>>()),
            )
        };

        // Off, then 60..=95 °C in 5 °C steps.
        let steps: Vec<Option<u8>> = std::iter::once(None)
            .chain((60..=95).step_by(5).map(Some))
            .collect();
        let sel = steps.iter().position(|t| *t == s.auto_boost).unwrap_or(0);
        let labels = steps
            .iter()
            .enumerate()
            .map(|(i, t)| (i, t.map_or("Off".to_string(), |v| v.to_string())))
            .collect();
        let slider = self.slider(
            cx,
            SliderSpec {
                id: "auto-boost",
                n: steps.len(),
                value: sel,
                fill: sel > 0,
                labels,
                cells: None,
            },
            move |i| {
                let t = steps[i];
                Box::new(move |hw| hw.set_auto_boost(t))
            },
        );
        let auto = section(p, "Auto-boost")
            .child(row(
                p,
                "Boost above",
                s.auto_boost
                    .map_or("off".to_string(), |t| format!("{t} °C")),
            ))
            .child(slider)
            .child(note(
                p,
                "Boost on at the threshold, off 8 °C lower. Runs while this app is open.",
            ));

        // Every profile has its own curves. Custom shows those of the running profile, to edit; Auto and Boost
        // show the Windows app's default for its mode, which cannot be changed.
        let running = s.profile.unwrap_or(Profile::Stock(OperatingMode::Gaming));
        let edit = s.fan.enabled.then_some(running);
        let (cpu_duty, gpu_duty) = if s.fan.enabled {
            (s.fan.cpu, s.fan.gpu)
        } else {
            (s.fan_default.cpu, s.fan_default.gpu)
        };
        let mut profile = section(p, &format!("Fan curves: {}", s.label(running)));
        if edit.is_some() {
            profile = profile.child(
                div().flex().gap_2().child(
                    button(
                        p,
                        "fan-default",
                        format!("Default preset ({})", s.mode_name(running)),
                        false,
                    )
                    .on_click(cx.listener(move |ui, _, _, cx| {
                        ui.act(cx, move |hw| hw.reset_fan_curves(running))
                    })),
                ),
            );
        }
        profile = profile.child(note(
            p,
            if edit.is_some() {
                format!("Drag the dots: duty at each temperature, linear in between. The curve never falls and stays at {} % or more from {} °C. Fan 1 follows the CPU temperature, fan 2 the GPU's; both run at 100 % from {} °C or without a sensor.", control::FAN_MIN_AT_TOP, control::FAN_ANCHORS[4], control::FAN_PANIC_C)
            } else {
                format!("The curves can only be changed in Custom mode. Shown here: the Windows app's default for {}, about what the firmware runs.", s.mode_name(running))
            },
        ));
        if s.fan.enabled && !s.service {
            profile = profile.child(
                div().p_3().rounded_md().bg(rgb(p.err_bg)).text_color(rgb(p.red)).child("The curves are applied by the ripple-nova service, which is not running (./install.sh)."),
            );
        }
        let cpu = self.fan_curve(cx, "Fan 1 (CPU) curve", 0, cpu_duty, edit);
        let gpu = self.fan_curve(cx, "Fan 2 (GPU) curve", 1, gpu_duty, edit);

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(mode)
            .child(fans)
            .child(profile)
            .child(cpu)
            .child(gpu)
            .child(auto)
            .into_any_element()
    }

    /// One fan's duty curve (fan 0 = CPU, 1 = GPU) as a chart. With `edit` (the profile whose curve it
    /// is) you drag a dot up or down, or press anywhere to take the dot of the nearest column; every dot
    /// is a vertical slider over the same plot, so the debounce, the release and the hardware job are the
    /// sliders'. Without it the curve is only shown, dimmed.
    pub(crate) fn fan_curve(
        &self,
        cx: &mut Context<Self>,
        title: &str,
        fan: usize,
        duty: [u8; 5],
        edit: Option<Profile>,
    ) -> Div {
        const H: f32 = 180.;
        const AXIS_W: f32 = 40.;
        const GUTTER_W: f32 = 32.;
        let p = self.p();
        let ids = FAN_IDS[fan];
        let bounds = self.sliders.borrow().get(ids[0]).and_then(|r| r.bounds);
        for (i, d) in duty.into_iter().enumerate().filter(|_| edit.is_some()) {
            let profile = edit.unwrap_or(Profile::Stock(OperatingMode::Gaming));
            self.sliders.borrow_mut().insert(
                ids[i],
                SliderReg {
                    bounds,
                    n: 101,
                    value: usize::from(d),
                    pick: Rc::new(move |k| {
                        Box::new(move |hw| hw.set_fan_point(profile, fan, i, k as u8))
                    }),
                    vertical: true,
                    // the last point never goes below the floor the profile demands
                    lo: if i == 4 {
                        usize::from(control::FAN_MIN_AT_TOP)
                    } else {
                        0
                    },
                },
            );
        }
        // while dragging, show the curve the way it will be saved: the other dots move along
        let held = edit
            .and(self.slider.thumb)
            .and_then(|(id, k)| ids.iter().position(|x| *x == id).map(|i| (i, k)));
        let mut shown = duty;
        if let Some((i, k)) = held {
            control::set_curve_point(&mut shown, i, k as u8);
        }

        let regs = self.sliders.clone();
        let plot = div()
            .id(("fan-chart", fan))
            .relative()
            .flex_1()
            .h(px(H))
            .when_some(edit, |d, _| {
                d.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ui, e: &MouseDownEvent, _, cx| {
                        ui.chart_press(fan, (f32::from(e.position.x), f32::from(e.position.y)), cx)
                    }),
                )
            })
            .child(
                canvas(
                    move |b, _, _| {
                        for id in ids {
                            if let Some(r) = regs.borrow_mut().get_mut(id) {
                                r.bounds = Some(b);
                            }
                        }
                    },
                    move |b, _, window, _| {
                        paint_chart(window, b, p, &shown, held.map(|(i, _)| i), edit.is_none())
                    },
                )
                .absolute()
                .size_full(),
            );
        let label = |text: String| {
            div()
                .text_size(px(TEXT_CAPTION))
                .text_color(rgb(p.muted))
                .child(text)
        };
        let y_axis = div()
            .relative()
            .w(px(AXIS_W))
            .h(px(H))
            .children((0..=5).map(|k| {
                let d = k * 20;
                div()
                    .absolute()
                    .right_0()
                    .top(px((1. - d as f32 / 100.) * H - 7.))
                    .child(label(format!("{d} %")))
            }));
        let x_axis = div()
            .relative()
            .flex_1()
            .h(px(16.))
            .children((30..=100).step_by(10).map(|t| {
                div()
                    .absolute()
                    .left(relative(chart::frac_x(t as f32)))
                    .ml(px(-20.))
                    .w(px(40.))
                    .flex()
                    .justify_center()
                    .child(label(t.to_string()))
            }));
        // each dot's exact value under its own column: the duty, then the temperature it is set at
        let values = div().relative().flex_1().h(px(32.)).children(
            control::FAN_ANCHORS.iter().zip(shown).map(|(t, d)| {
                div()
                    .absolute()
                    .left(relative(chart::frac_x(f32::from(*t))))
                    .ml(px(-24.))
                    .w(px(48.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(div().text_size(px(TEXT_NOTE)).child(format!("{d} %")))
                    .child(label(format!("{t} °C")))
            }),
        );
        section(p, title).child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                // the gutter right of the plot keeps the last tick inside the section and holds
                // the unit of the x axis
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(y_axis)
                        .child(plot)
                        .child(div().w(px(GUTTER_W))),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().w(px(AXIS_W)))
                        .child(x_axis)
                        .child(div().w(px(GUTTER_W)).pl(px(10.)).child(label("°C".into()))),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().w(px(AXIS_W)))
                        .child(values)
                        .child(div().w(px(GUTTER_W))),
                ),
        )
    }
}
