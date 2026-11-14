//! The Battery tab: the battery charge mode and the battery's readings.

use crate::*;
use ripple_hw::linux::CHARGE_PROFILES;

impl Ui {
    pub(crate) fn battery_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let Some(b) = &self.snap.battery else {
            return section(p, "Battery charge mode")
                .child(note(
                    p,
                    "No charging interface (is the uniwill kernel driver loaded?).",
                ))
                .into_any_element();
        };

        // exactly what the kernel offers, in its order (Trickle, Standard, Long_Life); the OEM's names only
        // appear in the descriptions
        let mut buttons = div().flex().gap_2();
        let mut words = section(p, "What they do");
        for c in b
            .available
            .iter()
            .filter_map(|a| CHARGE_PROFILES.iter().find(|c| c.kernel == a))
        {
            let kernel = c.kernel;
            buttons = buttons.child(button(p, kernel, c.label, b.selected == kernel).on_click(
                cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_charge_type(kernel))),
            ));
            words = words.child(note(p, format!("{} (OEM: {}): {}", c.label, c.oem, c.info)));
        }
        let mode = section(p, "Battery charge mode").child(buttons).child(note(
            p,
            "Protect the battery from high voltage. The firmware keeps the limit; it is not visible to the operating system.",
        ));

        // current_now is the only honest reading: with a mode on, the EC reports "Full" or "Charging" as it likes
        let current = b.current_ua.map_or("n/a".to_string(), |ua| {
            if ua == 0 {
                "0.00 A (not charging)".to_string()
            } else {
                format!("{:.2} A", f64::from(ua.unsigned_abs()) / 1_000_000.0)
            }
        });
        let readings = section(p, "Battery")
            .child(row(
                p,
                "Charge",
                b.capacity.map_or("n/a".to_string(), |c| format!("{c} %")),
            ))
            .child(row(p, "Status", b.status.clone()))
            .child(row(p, "Health", b.health.clone()))
            .child(row(p, "Current", current))
            .child(note(
                p,
                "With a mode other than Standard the status can read Full or Charging while no current flows: trust the current.",
            ));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(mode)
            .child(readings)
            .child(words)
            .into_any_element()
    }
}
