//! The Keyboard tab: colour, brightness, rainbow.

use crate::*;

impl Ui {
    pub(crate) fn keyboard_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        if !s.has_keyboard {
            return section(p, "Keyboard backlight: unavailable")
                .child(note(p, "No RGB keyboard reported by the EC."))
                .into_any_element();
        }
        let hsv = s.hsv;
        let shown = hsv_hex(hsv.h, hsv.s, hsv.v);

        // Presets set hue and saturation; the brightness you chose is kept.
        let colours: [(u16, u8); 9] = [
            (0, 100),
            (30, 100),
            (60, 100),
            (120, 100),
            (180, 100),
            (240, 100),
            (270, 80),
            (300, 100),
            (0, 0),
        ];
        let swatches = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(colours.iter().enumerate().map(|(i, (h, sat))| {
                let (h, sat) = (*h, *sat);
                div()
                    .id(("kb-swatch", i))
                    .size_8()
                    .rounded_md()
                    .bg(rgb(hsv_hex(h, sat, 100)))
                    .border_1()
                    .border_color(rgb(p.border))
                    .cursor_pointer()
                    .on_click(cx.listener(move |ui, _, _, cx| {
                        let v = ui.snap.hsv.v;
                        ui.act(cx, move |hw| hw.set_keyboard(Hsv { h, s: sat, v }));
                    }))
            }));

        let hue = self.slider(
            cx,
            SliderSpec {
                id: "kb-hue",
                n: 36,
                value: usize::from(hsv.h / 10) % 36,
                fill: false,
                labels: vec![(0, "0°".into()), (18, "180°".into()), (35, "350°".into())],
                cells: Some((0..36).map(|i| hsv_hex(i * 10, 100, 100)).collect()),
            },
            |i| {
                Box::new(move |hw| {
                    let cur = hw.hsv();
                    hw.set_keyboard(Hsv {
                        h: i as u16 * 10,
                        s: if cur.s == 0 { 100 } else { cur.s },
                        v: cur.v,
                    })
                })
            },
        );
        let sat = self.slider(
            cx,
            SliderSpec {
                id: "kb-sat",
                n: 11,
                value: (usize::from(hsv.s) + 5) / 10,
                fill: false,
                labels: vec![(0, "0 %".into()), (10, "100 %".into())],
                cells: Some((0..=10).map(|i| hsv_hex(hsv.h, i * 10, 100)).collect()),
            },
            |i| {
                Box::new(move |hw| {
                    let cur = hw.hsv();
                    hw.set_keyboard(Hsv {
                        s: i as u8 * 10,
                        ..cur
                    })
                })
            },
        );
        let bright = self.slider(
            cx,
            SliderSpec {
                id: "kb-bright",
                n: 21,
                value: (usize::from(hsv.v) + 2) / 5,
                fill: true,
                labels: vec![(0, "0".into()), (10, "50".into()), (20, "100".into())],
                cells: None,
            },
            |i| {
                Box::new(move |hw| {
                    let cur = hw.hsv();
                    hw.set_keyboard(Hsv {
                        v: i as u8 * 5,
                        ..cur
                    })
                })
            },
        );

        let effect = section(p, "Effect")
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button(p, "kb-static", "Colour", s.kb_level > 0 && !s.rainbow).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_rainbow(false)))))
                    .child(button(p, "kb-rainbow", "Rainbow", s.kb_level > 0 && s.rainbow).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_rainbow(true)))))
                    .child(button(p, "kb-off", "Off", s.kb_level == 0).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_keyboard_off())))),
            )
            .child(note(p, "Rainbow is the firmware's own static effect. Off is the backlight level, so the Fn keys bring it back too; picking a colour lights it again."));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(effect)
            .child(
                section(p, "Colour")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().size_8().rounded_md().bg(rgb(shown)).border_1().border_color(rgb(p.border)))
                            .child(
                                self.text_field(InputId::Hex, "hex-input", 110., "#rrggbb".into(), cx)
                                    .on_mouse_down_out(cx.listener(|ui, _, window, cx| {
                                        if ui.hex_focused {
                                            window.blur();
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .child(note(p, "Type #rrggbb and press Enter. The EC has 51 levels per channel, so the colour snaps a little."))
                    .child(swatches),
            )
            .child(
                section(p, "Picker")
                    .child(row(p, "Hue", format!("{}°", hsv.h)))
                    .child(hue)
                    .child(row(p, "Saturation", format!("{} %", hsv.s)))
                    .child(sat),
            )
            .child(
                section(p, "Brightness")
                    .child(row(p, "Level", format!("{} %", hsv.v)))
                    .child(bright)
                    .child(note(p, "No separate brightness register: this dims the colour itself. The Fn brightness keys still work on top.")),
            )
            .into_any_element()
    }
}
