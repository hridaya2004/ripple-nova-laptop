//! The Profiles tab: the profile dropdown and the editor of your own profiles.

use crate::*;

impl Ui {
    /// Select-style popover: trigger and list share one width, columns line up, the list floats
    /// over the page (it never pushes content), and a click anywhere else closes it.
    pub(crate) fn profile_dropdown(&self, cx: &mut Context<Self>) -> Div {
        const W: f32 = 400.;
        const NAME_W: f32 = 96.;
        const CPU_W: f32 = 150.;
        let p = self.p();
        let s = &self.snap;
        let open = self.dropdown_open;
        let current = s
            .profile
            .map_or_else(|| "Unknown".to_string(), |p| s.label(p));

        let dot = move |on: bool| {
            div()
                .size(px(8.))
                .rounded_full()
                .when(on, |d| d.bg(rgb(p.accent)))
        };
        let trigger = div()
            .id("profile-dropdown")
            .w(px(W))
            .h(px(32.))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .rounded_md()
            .border_1()
            .border_color(rgb(if open { p.accent } else { p.border }))
            .bg(rgb(p.panel))
            .hover(move |st| st.bg(rgb(p.hover)))
            .cursor_pointer()
            // State captured at render time: this handler and the list's click-away handler can
            // both run for the same press, and either order must end up closed.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ui, _, _, cx| {
                    ui.dropdown_open = !open;
                    ui.preview = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(dot(s.profile.is_some()))
                    .child(current),
            )
            .child(
                div()
                    .text_color(rgb(p.muted))
                    .child(if open { "▴" } else { "▾" }),
            );

        let mut wrap = div().relative().w(px(W)).child(trigger);
        if open {
            let header = div()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .h(px(24.))
                .text_size(px(TEXT_CAPTION))
                .text_color(rgb(p.muted))
                .child(div().w(px(8.)))
                .child(div().w(px(NAME_W)).child("PROFILE"))
                .child(div().w(px(CPU_W)).child("CPU PL1 / PL2 / PL4"))
                .child(div().child("GPU cTGP"));
            let mut list = div()
                .absolute()
                .top(px(36.))
                .left_0()
                .w(px(W))
                .flex()
                .flex_col()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(p.border))
                .bg(rgb(p.popup))
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(|ui, _, _, cx| {
                    ui.dropdown_open = false;
                    ui.preview = None;
                    cx.notify();
                }))
                .child(header);
            let entries: Vec<Profile> = STOCK_PROFILES
                .into_iter()
                .chain((0..s.customs.len()).map(Profile::Custom))
                .collect();
            for (i, m) in entries.into_iter().enumerate() {
                let pr = s.preset(m);
                let active = s.profile == Some(m);
                list = list.child(
                    div()
                        .id(("profile-option", i))
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(move |st| st.bg(rgb(p.hover)))
                        .when(active, |d| d.text_color(rgb(p.accent)))
                        .on_hover(cx.listener(move |ui, hovered: &bool, _, cx| {
                            if *hovered {
                                ui.preview = Some(m);
                            } else if ui.preview == Some(m) {
                                ui.preview = None;
                            }
                            cx.notify();
                        }))
                        .on_click(cx.listener(move |ui, _, _, cx| {
                            ui.dropdown_open = false;
                            ui.preview = None;
                            ui.act(cx, move |hw| hw.set_profile(m));
                        }))
                        .child(dot(active))
                        .child(div().w(px(NAME_W)).truncate().child(s.label(m)))
                        .child(
                            div()
                                .w(px(CPU_W))
                                .when(!active, |d| d.text_color(rgb(p.muted)))
                                .child(format!("{} / {} / {} W", pr.pl1, pr.pl2, pr.pl4)),
                        )
                        .child(
                            div()
                                .when(!active, |d| d.text_color(rgb(p.muted)))
                                .child(format!("+{} W", pr.ctgp)),
                        ),
                );
            }
            // a copy of what is running now, to edit
            list = list.child(
                div()
                    .id("profile-new")
                    .h(px(30.))
                    .px_2()
                    .mt_1()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .border_t_1()
                    .border_color(rgb(p.border))
                    .text_color(rgb(p.accent))
                    .hover(move |st| st.bg(rgb(p.hover)))
                    .on_click(cx.listener(|ui, _, _, cx| {
                        ui.dropdown_open = false;
                        ui.preview = None;
                        ui.act(cx, |hw| hw.new_profile());
                    }))
                    .child("+ New profile"),
            );
            // `deferred` paints the list after everything else, so later sections cannot cover it.
            wrap = wrap.child(deferred(list).with_priority(1));
        }
        wrap
    }

    /// A watt slider (5 W steps) for one value of the profile at `idx`.
    pub(crate) fn custom_slider(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        idx: usize,
        field: Field,
        value: u8,
        range: std::ops::RangeInclusive<u8>,
    ) -> Stateful<Div> {
        let (lo, hi) = (*range.start(), *range.end());
        let steps: Vec<u8> = (lo..=hi).step_by(5).collect();
        let sel = steps
            .iter()
            .position(|v| *v >= value)
            .unwrap_or(steps.len() - 1);
        let labels = vec![(0, lo.to_string()), (steps.len() - 1, hi.to_string())];
        self.slider(
            cx,
            SliderSpec {
                origin: None,
                id,
                n: steps.len(),
                value: sel,
                fill: true,
                labels,
                cells: None,
            },
            move |i| {
                let v = steps[i];
                Box::new(move |hw| hw.set_custom(idx, field, v))
            },
        )
    }

    pub(crate) fn profiles_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        let target = self.preview.or(s.profile);

        // What the hovered (or, when closed, the active) profile sets.
        let mut changes = section(
            p,
            &match target {
                Some(m) if Some(m) != s.profile => format!("Switch to {}", s.label(m)),
                Some(m) => s.label(m),
                None => "Current limits".to_string(),
            },
        );
        match target {
            Some(m) => {
                let pr = s.preset(m);
                changes = changes
                    .child(change(
                        p,
                        "CPU PL1 (Sustained Power)",
                        s.pl.map(|v| v.0),
                        pr.pl1,
                        "",
                    ))
                    .child(change(
                        p,
                        "CPU PL2 (Boost Power)",
                        s.pl.map(|v| v.1),
                        pr.pl2,
                        "",
                    ))
                    .child(change(p, "CPU PL4 (Peak Power)", s.pl4, pr.pl4, ""))
                    .child(change(p, "GPU cTGP", s.ctgp, pr.ctgp, "+"));
                match m {
                    Profile::Stock(OperatingMode::Turbo) => {
                        changes = changes.child(note(p, "The highest stock profile (the EC's own defaults). Runs hot under load."));
                    }
                    Profile::Custom(_) => {
                        changes = changes.child(note(p, "One of your own profiles, edited below."))
                    }
                    Profile::Stock(_) => {}
                }
            }
            None => {
                if let Some((a, b)) = s.pl {
                    let limits = match s.pl4 {
                        Some(c) => format!("{a} / {b} / {c} W"),
                        None => format!("{a} / {b} W"),
                    };
                    changes = changes.child(row(p, "CPU PL1 / PL2 / PL4", limits));
                }
            }
        }

        // Your own profiles are edited while one of them is running; the stock three are fixed.
        let custom = match s.profile {
            Some(Profile::Custom(i)) if i < s.customs.len() => {
                let c = &s.customs[i];
                let (pr, base) = (c.preset, Profile::Stock(c.base).name());
                section(p, &format!("{} (from {base})", c.name))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button(p, "custom-reset", format!("Reset to {base}"), false)
                                    .on_click(cx.listener(move |ui, _, _, cx| {
                                        ui.act(cx, move |hw| hw.reset_custom(i))
                                    })),
                            )
                            .child(
                                button(
                                    p,
                                    "custom-delete",
                                    if self.confirm_delete == Some(i) {
                                        "Click again to delete"
                                    } else {
                                        "Delete"
                                    },
                                    self.confirm_delete == Some(i),
                                )
                                .on_click(cx.listener(move |ui, _, _, cx| ui.delete_clicked(i, cx))),
                            ),
                    )
                    .child(row(p, "CPU PL1 (Sustained Power)", format!("{} W", pr.pl1)))
                    .child(self.custom_slider(cx, "custom-pl1", i, Field::Pl1, pr.pl1, 5..=control::PL_CEILING_W))
                    .child(row(p, "CPU PL2 (Boost Power)", format!("{} W", pr.pl2)))
                    .child(self.custom_slider(cx, "custom-pl2", i, Field::Pl2, pr.pl2, 5..=control::PL_CEILING_W))
                    .child(row(p, "CPU PL4 (Peak Power)", format!("{} W", pr.pl4)))
                    .child(self.custom_slider(cx, "custom-pl4", i, Field::Pl4, pr.pl4, 5..=control::PL4_CEILING_W))
                    .child(row(p, "GPU cTGP", format!("+{} W", pr.ctgp)))
                    .child(self.custom_slider(cx, "custom-ctgp", i, Field::Ctgp, pr.ctgp, 0..=control::CTGP_CEILING_W))
                    .child(note(p, format!("Changes apply at once. PL1 never above PL2, PL2 never above PL4. Capped at {} W (PL4 {} W, cTGP {} W).", control::PL_CEILING_W, control::PL4_CEILING_W, control::CTGP_CEILING_W)))
            }
            running => section(p, "Your own profiles")
                .child(
                    div().flex().gap_2().child(
                        button(
                            p,
                            "custom-new",
                            running.map_or("New profile".to_string(), |r| {
                                format!("New profile from {}", s.label(r))
                            }),
                            false,
                        )
                        .on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.new_profile()))),
                    ),
                )
                .child(note(p, "Office, Gaming and Turbo are fixed. A new profile starts as a copy of the one running now; you edit its limits here.")),
        };

        let mut page = div().flex().flex_col().gap_3();
        if let Some(e) = &s.ec_error {
            page = page.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(p.err_bg))
                    .text_color(rgb(p.red))
                    .child(format!("EC unavailable: {e}")),
            );
        }
        let profile = section(p, "Profile")
            .child(self.profile_dropdown(cx))
            .child(note(
            p,
            "The mode button cycles Office → Gaming → Turbo; lights 0 / 1 / 2 (your own profiles show 1).",
        ));
        page.child(profile)
            .child(changes)
            .child(custom)
            .into_any_element()
    }
}
