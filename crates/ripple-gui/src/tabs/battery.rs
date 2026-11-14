//! The Battery tab: the charging profile and the battery's readings.

use crate::*;
use ripple_hw::linux::CHARGE_PROFILES;

const IDS: [&str; 3] = ["charge-standard", "charge-long-life", "charge-trickle"];

impl Ui {
    pub(crate) fn battery_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let Some(b) = &self.snap.battery else {
            return section(p, "Charging profile")
                .child(note(
                    p,
                    "No charging interface (is the uniwill kernel driver loaded?).",
                ))
                .into_any_element();
        };

        let mut buttons = div().flex().gap_2();
        let mut words = section(p, "What they do");
        for (i, c) in CHARGE_PROFILES.iter().enumerate() {
            let kernel = c.kernel;
            let on = b.selected.eq_ignore_ascii_case(kernel);
            buttons = buttons.child(
                button(p, IDS[i], format!("{} ({})", c.label, c.oem), on).on_click(cx.listener(
                    move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_charge_type(kernel)),
                )),
            );
            words = words.child(note(p, format!("{} ({}): {}", c.label, c.oem, c.info)));
        }
        let profile = section(p, "Charging profile")
            .child(buttons)
            .child(note(
                p,
                "Protect the battery from high voltage. The firmware keeps the limit; it is not visible to the operating system.",
            ));

        // current_now is the only honest reading: with a profile on, the EC reports "Full" or "Charging" as it likes
        let current = b.current_ua.map_or("n/a".to_string(), |ua| {
            let a = f64::from(ua.unsigned_abs()) / 1_000_000.0;
            if ua == 0 {
                "0.00 A (not charging)".to_string()
            } else {
                format!("{a:.2} A")
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
                "With a profile other than Standard the status can read Full or Charging while no current flows: trust the current.",
            ));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(profile)
            .child(readings)
            .child(words)
            .into_any_element()
    }
}
