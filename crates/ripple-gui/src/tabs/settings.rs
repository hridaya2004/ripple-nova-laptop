//! The Settings tab: theme, font, notifications, startup.

use crate::*;

impl Ui {
    /// Font select: same popover pattern as the profile dropdown. Every installed family is listed
    /// (virtualized, so hundreds of entries stay fast) and drawn in its own font. The first entry
    /// is the default, the system monospace font.
    /// The font field. While it is being edited the typeahead floats over what is below it and takes no
    /// layout space (`deferred`, like the profile dropdown): the page does not move.
    pub(crate) fn font_field(&self, cx: &mut Context<Self>) -> Div {
        const W: f32 = 400.;
        let p = self.p();
        let default_label = format!("System monospace ({})", self.system_font);
        let wrap = div()
            .relative()
            .w(px(W))
            // clicking anywhere else ends the editing; clicks on the rows below are inside
            .on_mouse_down_out(cx.listener(|ui, _, window, cx| {
                if ui.font_focused {
                    window.blur();
                    cx.notify();
                }
            }))
            .child(self.text_field(InputId::Font, "font-input", W, default_label.clone(), cx));
        if !self.font_focused {
            return wrap;
        }
        let (rows, total) = self.font_rows();
        let sel = self.font_sel.min(rows.len().saturating_sub(1));
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
            // the popup covers the sections below: their controls must not get its clicks
            .occlude();
        for (i, row) in rows.iter().enumerate() {
            let (label, family): (String, SharedString) = match row {
                None => (default_label.clone(), self.system_font.clone()),
                Some(n) => (n.clone(), n.clone().into()),
            };
            let pick = row.clone();
            list = list.child(
                div()
                    .id(("font", i))
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(i == sel, |d| d.bg(rgb(p.hover)))
                    .hover(move |st| st.bg(rgb(p.hover)))
                    // on mouse down: the row must still be there when the button is released
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ui, _, window, cx| {
                            ui.pick_font(pick.clone(), window, cx)
                        }),
                    )
                    .child(div().truncate().font_family(family).child(label)),
            );
        }
        if rows.is_empty() {
            list = list.child(
                div()
                    .h(px(26.))
                    .px_2()
                    .text_color(rgb(p.muted))
                    .child("No installed font matches"),
            );
        }
        let typed = !self.font.text().trim().is_empty();
        let count = if typed {
            format!("{total} matches, showing {}", rows.len())
        } else {
            format!("{total} families installed")
        };
        // the count sits inside the popup: nothing outside it moves
        list = list.child(note(p, count).px_2().pt_1());
        // `deferred` paints the popup after everything else, so the sections below cannot cover it
        wrap.child(deferred(list).with_priority(1))
    }

    pub(crate) fn settings_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let light = self.settings.light;

        let theme = section(p, "Theme").child(
            div()
                .flex()
                .gap_2()
                .child(
                    button(p, "theme-dark", "Dark", !light).on_click(cx.listener(
                        |ui, _, _, cx| {
                            ui.settings.light = false;
                            ui.save_settings(cx);
                        },
                    )),
                )
                .child(
                    button(p, "theme-light", "Light", light).on_click(cx.listener(
                        |ui, _, _, cx| {
                            ui.settings.light = true;
                            ui.save_settings(cx);
                        },
                    )),
                ),
        );

        let font = section(p, "Font")
            .child(self.font_field(cx))
            .child(
                div()
                    .font_family(self.family())
                    .child("The quick brown fox 0123456789"),
            )
            .child(note(
                p,
                "Type to search the installed fonts; Up/Down and Enter or a click picks. Empty is the system monospace font.",
            ));

        let notifications = section(p, "Notifications").child(self.switch_row(
            cx,
            "popup-toggle",
            "Show popup",
            "Notify when the performance profile, fan boost or a toggle changes. The tray menu switches this too.",
            self.settings.popups,
            |ui, cx| {
                let on = !ui.settings.popups;
                match ripple_hw::notify::set_popups(on) {
                    Ok(()) => ui.settings.popups = on,
                    Err(e) => ui.say(format!("error: could not save the popup setting: {e}"), cx),
                }
                cx.notify();
            },
        ));
        let startup = section(p, "Startup")
            .child(self.switch_row(
                cx,
                "autostart",
                "Open at login",
                "Starts the window and the tray icon when you log in (no password: the sudoers rule from install.sh, else polkit asks).",
                self.autostart,
                |ui, cx| ui.toggle_autostart(cx),
            ))
            .child(self.switch_row(
                cx,
                "start-min",
                "Start minimized",
                "At login only the tray icon starts, which opens the window. The service always starts at boot.",
                self.settings.start_minimized,
                |ui, cx| ui.toggle_start_minimized(cx),
            ));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(theme)
            .child(font)
            .child(notifications)
            .child(startup)
            .into_any_element()
    }
}
