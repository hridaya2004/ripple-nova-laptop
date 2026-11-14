//! Sliders (and the fan chart's dots, which are vertical sliders): positions, drag, and the debounced hardware job.

use crate::*;

/// What the pointer handlers need to know about a slider, refreshed on every render.
pub(crate) struct SliderReg {
    /// Screen bounds of the track, recorded while painting.
    pub(crate) bounds: Option<Bounds<Pixels>>,
    pub(crate) n: usize,
    pub(crate) value: usize,
    pub(crate) pick: Pick,
    /// A vertical track (the fan chart's dots): the top is the highest position, and the pointer's
    /// height is what counts.
    pub(crate) vertical: bool,
    /// Lowest position the pointer can pick (the last fan point has a floor).
    pub(crate) lo: usize,
}

pub(crate) struct SliderSpec {
    pub(crate) id: &'static str,
    /// Number of positions (steps + 1).
    pub(crate) n: usize,
    pub(crate) value: usize,
    /// Fill the track up to the thumb.
    pub(crate) fill: bool,
    /// `(position, text)` shown under the track.
    pub(crate) labels: Vec<(usize, String)>,
    /// Paint the track as coloured cells instead (hue / saturation pickers).
    pub(crate) cells: Option<Vec<u32>>,
    /// A slider with a zero in the middle (an offset): the position of the zero. The fill then runs from it to the
    /// thumb, in the accent colour above it and in yellow below it, and the zero carries a mark.
    pub(crate) origin: Option<usize>,
}

/// Which of `n` positions a pointer at `x` is over; the track spans `left..left + width`.
pub(crate) fn step_at(x: f32, left: f32, width: f32, n: usize) -> usize {
    if n < 2 || width <= 0.0 {
        return 0;
    }
    let frac = ((x - left) / width).clamp(0.0, 1.0);
    (frac * (n - 1) as f32).round() as usize
}

/// `step_at` for a vertical track spanning `top..top + height`: the top is the last position.
pub(crate) fn step_at_y(y: f32, top: f32, height: f32, n: usize) -> usize {
    step_at(top + height - y, 0.0, height, n)
}

/// The slider ids of each fan's five curve dots, CPU fan then GPU fan.
pub(crate) const FAN_IDS: [[&str; 5]; 2] = [
    [
        "fan-cpu-0",
        "fan-cpu-1",
        "fan-cpu-2",
        "fan-cpu-3",
        "fan-cpu-4",
    ],
    [
        "fan-gpu-0",
        "fan-gpu-1",
        "fan-gpu-2",
        "fan-gpu-3",
        "fan-gpu-4",
    ],
];

impl Ui {
    /// `(position, hardware value, job builder)` for a pointer at `(x, y)` over slider `id`.
    pub(crate) fn step_for(
        &self,
        id: &'static str,
        (x, y): (f32, f32),
    ) -> Option<(usize, usize, Pick)> {
        let reg = self.sliders.borrow();
        let r = reg.get(id)?;
        let b = r.bounds?;
        let step = if r.vertical {
            step_at_y(y, f32::from(b.origin.y), f32::from(b.size.height), r.n)
        } else {
            step_at(x, f32::from(b.origin.x), f32::from(b.size.width), r.n)
        };
        Some((step.max(r.lo), r.value, r.pick.clone()))
    }

    pub(crate) fn slider_press(
        &mut self,
        id: &'static str,
        at: (f32, f32),
        cx: &mut Context<Self>,
    ) {
        if let Some((i, value, _)) = self.step_for(id, at) {
            let generation = self.slider.press(id, value, i);
            self.slider_timer(generation, cx);
            cx.notify();
        }
    }

    /// A press on a fan chart picks the dot whose column is nearest and drags it from there.
    pub(crate) fn chart_press(&mut self, fan: usize, at: (f32, f32), cx: &mut Context<Self>) {
        let ids = FAN_IDS[fan];
        let Some(b) = self.sliders.borrow().get(ids[0]).and_then(|r| r.bounds) else {
            return;
        };
        let frac = (at.0 - f32::from(b.origin.x)) / f32::from(b.size.width);
        self.slider_press(ids[chart::nearest_anchor(frac)], at, cx);
    }

    pub(crate) fn slider_drag(&mut self, at: (f32, f32), cx: &mut Context<Self>) {
        let Some((id, last)) = self.slider.thumb.filter(|_| self.slider.down) else {
            return;
        };
        if let Some((i, _, _)) = self.step_for(id, at) {
            if i != last {
                let generation = self.slider.moved(id, i);
                self.slider_timer(generation, cx);
                cx.notify();
            }
        }
    }

    /// The pointer went up: send the final position (unless the hardware already has it).
    pub(crate) fn slider_release(&mut self, cx: &mut Context<Self>) {
        if let Some((id, i)) = self.slider.release() {
            self.slider_send(id, i, cx);
        }
        self.slider.settle();
        cx.notify();
    }

    /// Debounce: send the thumb's position only if the pointer has not moved for `drag::DEBOUNCE`.
    pub(crate) fn slider_timer(&mut self, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(drag::DEBOUNCE).await;
            this.update(cx, |ui, cx| {
                if let Some((id, i)) = ui.slider.due(generation) {
                    ui.slider_send(id, i, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn slider_send(&mut self, id: &'static str, i: usize, cx: &mut Context<Self>) {
        let pick = self.sliders.borrow().get(id).map(|r| r.pick.clone());
        if let Some(pick) = pick {
            self.run(cx, pick(i), true);
        }
    }

    /// A stepped slider: track (or coloured cells), fill, ticks, a thumb and optional labels.
    /// Click anywhere on it or drag; the thumb snaps to the nearest step.
    pub(crate) fn slider(
        &self,
        cx: &mut Context<Self>,
        spec: SliderSpec,
        pick: impl Fn(usize) -> Job + 'static,
    ) -> Stateful<Div> {
        let p = self.p();
        let id = spec.id;
        let n = spec.n.max(2);
        let prev = self.sliders.borrow().get(id).and_then(|r| r.bounds);
        self.sliders.borrow_mut().insert(
            id,
            SliderReg {
                bounds: prev,
                n,
                value: spec.value,
                pick: Rc::new(pick),
                vertical: false,
                lo: 0,
            },
        );

        // while dragging show the thumb where the pointer is, not where the hardware last was
        let shown = match self.slider.thumb {
            Some((d, i)) if d == id => i,
            _ => spec.value,
        };
        let frac = |i: usize| i as f32 / (n - 1) as f32;
        let regs = self.sliders.clone();
        // an offset slider is blue above its zero and yellow below it, wherever the thumb is
        let tone = match spec.origin {
            Some(o) if shown < o => p.yellow,
            _ => p.accent,
        };

        let mut inner = div()
            .absolute()
            .left(px(10.))
            .right(px(10.))
            .top_0()
            .bottom_0()
            .child(
                canvas(
                    move |bounds, _, _| {
                        if let Some(r) = regs.borrow_mut().get_mut(id) {
                            r.bounds = Some(bounds);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );

        match &spec.cells {
            Some(cells) => {
                inner = inner.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(10.))
                        .h(px(10.))
                        .flex()
                        .rounded_sm()
                        .overflow_hidden()
                        .children(cells.iter().map(|c| div().flex_1().h_full().bg(rgb(*c)))),
                );
            }
            None => {
                inner = inner.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(12.))
                        .h(px(6.))
                        .rounded_full()
                        .bg(rgb(p.border)),
                );
                if let Some(o) = spec.origin {
                    // from the zero to the thumb: which side, and how far, is what an offset is
                    let (lo, hi) = (o.min(shown), o.max(shown));
                    inner = inner
                        .child(
                            div()
                                .absolute()
                                .left(relative(frac(lo)))
                                .top(px(12.))
                                .h(px(6.))
                                .w(relative(frac(hi) - frac(lo)))
                                .bg(rgb(tone)),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(relative(frac(o)))
                                .ml(px(-1.))
                                .top(px(8.))
                                .w(px(2.))
                                .h(px(14.))
                                .bg(rgb(p.fg)),
                        );
                } else if spec.fill {
                    inner = inner.child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(12.))
                            .h(px(6.))
                            .w(relative(frac(shown)))
                            .rounded_full()
                            .bg(rgb(p.accent)),
                    );
                }
                if n <= 24 {
                    for i in 0..n {
                        inner = inner.child(
                            div()
                                .absolute()
                                .left(relative(frac(i)))
                                .top(px(24.))
                                .w(px(1.))
                                .h(px(4.))
                                .bg(rgb(p.muted)),
                        );
                    }
                }
            }
        }
        inner = inner.child(
            div()
                .absolute()
                .left(relative(frac(shown)))
                .ml(px(-8.))
                .top(px(7.))
                .size(px(16.))
                .rounded_full()
                .bg(rgb(p.popup))
                .border_2()
                .border_color(rgb(tone))
                .shadow_sm(),
        );
        for (i, text) in &spec.labels {
            inner = inner.child(
                div()
                    .absolute()
                    .left(relative(frac(*i)))
                    .ml(px(-22.))
                    .w(px(44.))
                    .top(px(30.))
                    .flex()
                    .justify_center()
                    .text_size(px(TEXT_CAPTION))
                    .text_color(rgb(if *i == shown { p.fg } else { p.muted }))
                    .child(text.clone()),
            );
        }

        div()
            .id(id)
            .relative()
            .w_full()
            .h(px(if spec.labels.is_empty() { 32. } else { 50. }))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ui, e: &MouseDownEvent, _, cx| {
                    ui.slider_press(id, (f32::from(e.position.x), f32::from(e.position.y)), cx)
                }),
            )
            .child(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_snaps_to_the_nearest_step_and_clamps_at_the_ends() {
        // 5 positions across a 400 px track starting at x = 100 → steps every 100 px
        assert_eq!(step_at(100.0, 100.0, 400.0, 5), 0);
        assert_eq!(step_at(149.0, 100.0, 400.0, 5), 0);
        assert_eq!(step_at(151.0, 100.0, 400.0, 5), 1);
        assert_eq!(step_at(300.0, 100.0, 400.0, 5), 2);
        assert_eq!(step_at(500.0, 100.0, 400.0, 5), 4);
        assert_eq!(step_at(-50.0, 100.0, 400.0, 5), 0, "left of the track");
        assert_eq!(step_at(900.0, 100.0, 400.0, 5), 4, "right of the track");
        assert_eq!(step_at(250.0, 100.0, 0.0, 5), 0, "not laid out yet");
        assert_eq!(step_at(250.0, 100.0, 400.0, 1), 0, "single position");
    }

    #[test]
    fn a_vertical_track_counts_up_from_the_bottom() {
        // 101 positions over a 200 px track from y = 100 to 300: 2 px per percent
        assert_eq!(step_at_y(300.0, 100.0, 200.0, 101), 0, "bottom");
        assert_eq!(step_at_y(100.0, 100.0, 200.0, 101), 100, "top");
        assert_eq!(step_at_y(200.0, 100.0, 200.0, 101), 50, "middle");
        assert_eq!(step_at_y(500.0, 100.0, 200.0, 101), 0, "below the track");
        assert_eq!(step_at_y(0.0, 100.0, 200.0, 101), 100, "above the track");
    }
}
