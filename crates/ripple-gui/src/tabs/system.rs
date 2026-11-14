//! The System and Device tabs: platform switches.

use crate::*;

/// The toggles that belong to the Device tab: the touchpad's power switch.
pub(crate) const DEVICE_TOGGLES: [&str; 1] = ["touchpad"];

pub(crate) fn toggle_description(name: &str) -> &str {
    match name {
        "fn_lock" => "F-keys act as media keys unless Fn is held",
        "super_key_enable" => "The Super (Windows) key works",
        "ac_auto_boot" => "Power on when AC is plugged in (enabling it turns USB powershare off)",
        "usb_powershare_high" => {
            "Keep the USB ports powered when off (enabling it turns AC auto-boot off)"
        }
        "touchpad" => "Off powers the touchpad down: no pointer, its LED lights (as in suspend)",
        "breathing_in_suspend" => {
            "Breathing LED animation while suspended on AC power (a lightbar feature)"
        }
        "touchpad_toggle_enable" => {
            "The firmware's own touchpad toggle; no effect seen on this laptop"
        }
        _ => "",
    }
}

impl Ui {
    /// One on/off setting: its name, what it does, and the switch at the right. The same row everywhere.
    pub(crate) fn switch_row(
        &self,
        cx: &mut Context<Self>,
        id: impl Into<ElementId>,
        label: &str,
        what: &str,
        on: bool,
        toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Div {
        let p = self.p();
        spread(
            div()
                .child(label.to_string())
                .child(note(p, what.to_string())),
            switch(p, id, on).on_click(cx.listener(move |ui, _, _, cx| toggle(ui, cx))),
        )
    }

    /// One platform toggle: its name, what it does, and a switch. `i` is its place in the snapshot's
    /// toggle list.
    pub(crate) fn toggle_row(
        &self,
        cx: &mut Context<Self>,
        i: usize,
        name: &'static str,
        on: bool,
    ) -> Div {
        self.switch_row(
            cx,
            ("toggle", i),
            toggle_label(name),
            toggle_description(name),
            on,
            move |ui, cx| ui.act(cx, move |hw| hw.set_toggle(name, !on)),
        )
    }

    /// The touchpad's power switch.
    pub(crate) fn device_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let mine = self
            .snap
            .toggles
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, (n, _))| DEVICE_TOGGLES.contains(n))
            .collect::<Vec<_>>();
        let mut touchpad = section(p, "Touchpad");
        if mine.is_empty() {
            touchpad = touchpad.child(note(p, "No Uniwill touchpad found (no i2c-UNIW* device)."));
        } else {
            for (i, (name, on)) in mine {
                touchpad = touchpad.child(self.toggle_row(cx, i, name, on));
            }
            touchpad = touchpad.child(note(
                p,
                "With the touchpad off there is no pointer: `sudo -n rcc touchpad toggle`, bound to a keyboard shortcut, brings it back.",
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(touchpad)
            .into_any_element()
    }

    pub(crate) fn system_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        let mut toggles = section(p, "Toggles");
        let system_toggles = s
            .toggles
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, (n, _))| !DEVICE_TOGGLES.contains(n))
            .collect::<Vec<_>>();
        if system_toggles.is_empty() {
            toggles = toggles.child(note(p, "none"));
        }
        for (i, (name, on)) in system_toggles {
            toggles = toggles.child(self.toggle_row(cx, i, name, on));
        }

        let mut page = div().flex().flex_col().gap_3().child(toggles);
        if s.has_lightbar {
            let swatch = |i: usize, hex: u32| {
                let c = ((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
                div()
                    .id(("swatch", i))
                    .size_8()
                    .rounded_md()
                    .bg(rgb(hex))
                    .border_1()
                    .border_color(rgb(p.border))
                    .cursor_pointer()
                    .on_click(cx.listener(move |ui, _, _, cx| {
                        ui.act(cx, move |hw| hw.set_lightbar(Some(c)))
                    }))
            };
            let colours = [
                0xffffff, 0xff0000, 0xff8000, 0xffff00, 0x00ff00, 0x00ffff, 0x0000ff, 0xff00ff,
            ];
            page = page.child(
                section(p, "Lightbar")
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(colours.iter().enumerate().map(|(i, h)| swatch(i, *h))),
                    )
                    .child(button(p, "lb-off", "Off", false).on_click(
                        cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_lightbar(None))),
                    )),
            );
        } else {
            page = page.child(note(
                p,
                "No lightbar on this laptop (the EC reports none), so its controls are hidden.",
            ));
        }
        page.into_any_element()
    }
}
