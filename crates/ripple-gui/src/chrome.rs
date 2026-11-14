//! The window's frame: title bar, tab bar, status bar.

use crate::*;

impl Ui {
    pub(crate) fn titlebar(&self, window: &Window) -> impl IntoElement {
        let p = self.p();
        let inside = window.is_window_hovered();
        let model = if self.snap.model.is_empty() {
            "detecting…".to_string()
        } else {
            self.snap.model.clone()
        };
        div()
            .id("titlebar")
            .h(px(34.))
            .flex()
            .items_center()
            .justify_between()
            .bg(rgb(p.title))
            .border_b_1()
            .border_color(rgb(p.border))
            .on_mouse_down(MouseButton::Left, |e, window, _| {
                if e.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .pl_3()
                    .child(img(self.icon.clone()).size(px(18.)))
                    .child("Ripple Control Center")
                    .child(div().text_color(rgb(p.muted)).child(model)),
            )
            .child(
                div()
                    .flex()
                    .child(
                        window_button(
                            "w-min",
                            div().w(px(10.)).h(px(1.)).bg(rgb(p.fg)),
                            inside.then_some(p.hover),
                        )
                        .on_click(|_, window, _| window.minimize_window()),
                    )
                    .child(
                        window_button(
                            "w-max",
                            div().size(px(9.)).border_1().border_color(rgb(p.fg)),
                            inside.then_some(p.hover),
                        )
                        .on_click(|_, window, _| window.zoom_window()),
                    )
                    .child(
                        window_button(
                            "w-close",
                            div().text_size(px(16.)).child("×"),
                            inside.then_some(p.red),
                        )
                        .on_click(|_, window, _| window.remove_window()),
                    ),
            )
    }

    pub(crate) fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p();
        let tabs = [
            (Tab::Profiles, "Profiles"),
            (Tab::Fan, "Fan"),
            (Tab::Keyboard, "Keyboard"),
            (Tab::Device, "Device"),
            (Tab::System, "System"),
            (Tab::Settings, "Settings"),
        ];
        div()
            .flex()
            .h(px(34.))
            .bg(rgb(p.panel))
            .border_b_1()
            .border_color(rgb(p.border))
            .children(tabs.map(|(tab, label)| {
                let active = self.tab == tab;
                div()
                    .id(label)
                    .px_4()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .border_r_1()
                    .border_color(rgb(p.border))
                    .when(active, |d| d.bg(rgb(p.bg)).text_color(rgb(p.fg)))
                    .when(!active, |d| {
                        d.text_color(rgb(p.muted)).hover(|s| s.bg(rgb(p.hover)))
                    })
                    .on_click(cx.listener(move |ui, _, window, cx| {
                        ui.tab = tab;
                        ui.dropdown_open = false;
                        ui.confirm_delete = None;
                        window.blur(); // ends any text editing
                        cx.notify();
                    }))
                    .child(label)
            }))
    }

    pub(crate) fn status_bar(&self) -> impl IntoElement {
        let p = self.p();
        let s = &self.snap;
        let mut left = format!("backend {}", s.backend);
        if cfg!(debug_assertions) && !s.debug.is_empty() {
            left = format!("{left} · debug: {}", s.debug);
        }
        let (dot, label) = match (s.button, s.service) {
            (true, true) => ("●", "mode button (service)"),
            (true, false) => ("●", "mode button"),
            _ => ("○", "mode button off"),
        };
        div()
            .h(px(24.))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .bg(rgb(p.title))
            .border_t_1()
            .border_color(rgb(p.border))
            .text_size(px(TEXT_NOTE))
            .text_color(rgb(p.muted))
            .child(div().truncate().child(left))
            .child(div().truncate().child(s.event.clone()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(if s.button { p.green } else { p.muted }))
                    .child(format!("{dot} {label}")),
            )
    }

    // -------------------------------------------------------------- tabs
}
