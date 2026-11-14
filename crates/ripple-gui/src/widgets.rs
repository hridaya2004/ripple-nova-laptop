//! Small building blocks: button, switch, section, row, note, the window's own buttons.

use crate::*;

pub(crate) fn button(
    p: Palette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(if active { p.accent } else { p.border }))
        .bg(rgb(if active { p.accent_bg } else { p.panel }))
        .text_color(rgb(if active { p.accent } else { p.fg }))
        .hover(move |s| s.bg(rgb(p.hover)))
        .cursor_pointer()
        .child(label.into())
}

/// On/off switch: a track with a knob that sits right when on (gpui has no switch of its own).
pub(crate) fn switch(p: Palette, id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .border_1()
        .border_color(rgb(if on { p.accent } else { p.border }))
        .bg(rgb(if on { p.accent_bg } else { p.panel }))
        .when(on, |d| d.justify_end())
        .hover(move |s| s.bg(rgb(p.hover)))
        .cursor_pointer()
        .child(
            div()
                .size(px(14.))
                .rounded_full()
                .bg(rgb(if on { p.accent } else { p.muted })),
        )
}

/// A group of settings: a soft card (no outline, the colour sets it off) under a small heading.
pub(crate) fn section(p: Palette, title: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(rgb(p.panel))
        .child(
            div()
                .text_size(px(TEXT_CAPTION))
                .text_color(rgb(p.muted))
                .child(title.to_uppercase()),
        )
}

pub(crate) fn row(
    p: Palette,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_4()
        .child(label.into())
        .child(div().text_color(rgb(p.muted)).child(value.into()))
}

pub(crate) fn note(p: Palette, text: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(p.muted))
        .text_size(px(TEXT_NOTE))
        .child(text.into())
}

/// One line of the "what changes" panel: `PL1  35 W → 55 W` (or unchanged).
pub(crate) fn change(p: Palette, label: &str, from: Option<u8>, to: u8, prefix: &str) -> Div {
    let value = match from {
        Some(f) if f == to => format!("{prefix}{f} W"),
        Some(f) => format!("{prefix}{f} W → {prefix}{to} W"),
        None => format!("{prefix}{to} W"),
    };
    let mut r = row(p, label.to_string(), value);
    if matches!(from, Some(f) if f != to) {
        r = r.text_color(rgb(p.yellow));
    }
    r
}

// ------------------------------------------------------------------ window chrome

/// `hover` is off while the pointer is outside the window: GPUI gets no event when it leaves, so
/// the hover style of the button it left from would stay.
pub(crate) fn window_button(id: &'static str, glyph: Div, hover: Option<u32>) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(42.))
        .h(px(34.))
        .flex()
        .items_center()
        .justify_center()
        .when_some(hover, |d, c| d.hover(move |s| s.bg(rgb(c))))
        // Without this the titlebar's drag handler would start a window move on the click.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph)
}

pub(crate) fn resize_handles() -> Vec<Div> {
    let (e, c) = (px(5.), px(12.));
    let mk = |edge: ResizeEdge, d: Div| {
        d.absolute()
            .on_mouse_down(MouseButton::Left, move |_, w, _| {
                w.start_window_resize(edge)
            })
    };
    vec![
        mk(
            ResizeEdge::Top,
            div().top_0().left(c).right(c).h(e).cursor_ns_resize(),
        ),
        mk(
            ResizeEdge::Bottom,
            div().bottom_0().left(c).right(c).h(e).cursor_ns_resize(),
        ),
        mk(
            ResizeEdge::Left,
            div().left_0().top(c).bottom(c).w(e).cursor_ew_resize(),
        ),
        mk(
            ResizeEdge::Right,
            div().right_0().top(c).bottom(c).w(e).cursor_ew_resize(),
        ),
        mk(
            ResizeEdge::TopLeft,
            div().top_0().left_0().size(c).cursor_nwse_resize(),
        ),
        mk(
            ResizeEdge::BottomRight,
            div().bottom_0().right_0().size(c).cursor_nwse_resize(),
        ),
        mk(
            ResizeEdge::TopRight,
            div().top_0().right_0().size(c).cursor_nesw_resize(),
        ),
        mk(
            ResizeEdge::BottomLeft,
            div().bottom_0().left_0().size(c).cursor_nesw_resize(),
        ),
    ]
}
