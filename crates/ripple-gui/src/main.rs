//! `rcc-gui` — Ripple Control Center GPUI app. Root only (EC access).
//!
//! Zed-style window: client-side decorations (own titlebar, no GNOME bar), tabs, status bar,
//! One Dark / One Light palettes, a user-selectable font (system monospace by default). All
//! hardware I/O runs on GPUI's background executor behind one mutex, so the render path
//! never blocks.

mod autostart;
mod hw;
mod remote;
mod settings;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    canvas, deferred, div, prelude::*, px, relative, rgb, size, uniform_list, AnyElement, ScrollStrategy, UniformListScrollHandle, App, Application, Bounds,
    Context, Div, ElementId, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, ResizeEdge, SharedString, Stateful,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions,
};
use hw::{FanMode, Field, Hw, Snapshot, PROFILES};
use ripple_hw::notify::{ensure_tray, is_experimental, toggle_label};
use ripple_core::modes::OperatingMode;
use ripple_hw::control::{self, Hsv, Preset, Profile};
use settings::Settings;

/// Zed's One Dark / One Light, reduced to what this UI uses.
#[derive(Clone, Copy)]
struct Palette {
    title: u32,
    bg: u32,
    panel: u32,
    popup: u32,
    border: u32,
    hover: u32,
    fg: u32,
    muted: u32,
    accent: u32,
    accent_bg: u32,
    green: u32,
    red: u32,
    yellow: u32,
    err_bg: u32,
}

impl Palette {
    fn dark() -> Self {
        Self {
            title: 0x3b414d,
            bg: 0x282c34,
            panel: 0x2f343e,
            popup: 0x21252b,
            border: 0x464b57,
            hover: 0x363c46,
            fg: 0xdce0e5,
            muted: 0xa9afbc,
            accent: 0x74ade8,
            accent_bg: 0x2c3a4f,
            green: 0xa1c181,
            red: 0xd07277,
            yellow: 0xdec184,
            err_bg: 0x4a2c30,
        }
    }

    fn light() -> Self {
        Self {
            title: 0xdcdcdd,
            bg: 0xfafafa,
            panel: 0xebebec,
            popup: 0xffffff,
            border: 0xc9c9ca,
            hover: 0xdfdfe0,
            fg: 0x242529,
            muted: 0x58585a,
            accent: 0x3b5bdb,
            accent_bg: 0xdfe5fb,
            green: 0x4f8a3d,
            red: 0xd36151,
            yellow: 0x946a00,
            err_bg: 0xf6dcd8,
        }
    }
}

/// `rcc-gui [--tab profiles|fan|keyboard|system|settings] [--open] [--light] [--size WxH] [--minimized]`:
/// where to start (handy for screenshots; `--light` forces the light theme for this run).
/// `rcc-gui --autostart on|off|status` installs/removes/shows the open-at-login entry and exits.
#[derive(Clone, Copy)]
struct Start {
    tab: Tab,
    open: bool,
    light: bool,
    size: Option<(f32, f32)>,
    minimized: bool,
}

impl Start {
    /// A window opened later (by `show`): the defaults.
    fn plain() -> Self {
        Self { tab: Tab::Profiles, open: false, light: false, size: None, minimized: false }
    }

    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let tab = match args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).map(String::as_str) {
            Some("fan") => Tab::Fan,
            Some("keyboard") => Tab::Keyboard,
            Some("system") => Tab::System,
            Some("settings") => Tab::Settings,
            _ => Tab::Profiles,
        };
        let size = args.iter().position(|a| a == "--size").and_then(|i| args.get(i + 1)).and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        });
        Self {
            tab,
            open: args.iter().any(|a| a == "--open"),
            light: args.iter().any(|a| a == "--light"),
            size,
            minimized: args.iter().any(|a| a == "--minimized"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Profiles,
    Fan,
    Keyboard,
    System,
    Settings,
}

type Job = Box<dyn FnOnce(&mut Hw) -> Result<String, String> + Send>;
/// Builds the hardware job for a slider position.
type Pick = Rc<dyn Fn(usize) -> Job>;

// ------------------------------------------------------------------ slider

/// What the pointer handlers need to know about a slider, refreshed on every render.
struct SliderReg {
    /// Screen bounds of the track, recorded while painting.
    bounds: Option<Bounds<Pixels>>,
    n: usize,
    value: usize,
    pick: Pick,
}

struct SliderSpec {
    id: &'static str,
    /// Number of positions (steps + 1).
    n: usize,
    value: usize,
    /// Fill the track up to the thumb.
    fill: bool,
    /// `(position, text)` shown under the track.
    labels: Vec<(usize, String)>,
    /// Paint the track as coloured cells instead (hue / saturation pickers).
    cells: Option<Vec<u32>>,
}

/// Which of `n` positions a pointer at `x` is over; the track spans `left..left + width`.
fn step_at(x: f32, left: f32, width: f32, n: usize) -> usize {
    if n < 2 || width <= 0.0 {
        return 0;
    }
    let frac = ((x - left) / width).clamp(0.0, 1.0);
    (frac * (n - 1) as f32).round() as usize
}

/// The hardware and the latest snapshot. A background thread polls (this also runs the auto-boost policy)
/// and the window only reads `latest`.
struct Shared {
    hw: Arc<Mutex<Hw>>,
    snap: Mutex<Snapshot>,
}

impl Shared {
    fn publish(&self, snap: Snapshot) {
        *self.snap.lock().unwrap_or_else(|e| e.into_inner()) = snap;
    }

    fn latest(&self) -> Snapshot {
        self.snap.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

struct Ui {
    hw: Arc<Mutex<Hw>>,
    shared: Arc<Shared>,
    snap: Snapshot,
    tab: Tab,
    settings: Settings,
    system_font: SharedString,
    fonts: Vec<String>,
    dropdown_open: bool,
    font_open: bool,
    font_scroll: UniformListScrollHandle,
    /// Is the open-at-login entry installed?
    autostart: bool,
    /// Profile under the mouse in the dropdown, shown in the "what changes" panel.
    preview: Option<Profile>,
    sliders: Rc<RefCell<HashMap<&'static str, SliderReg>>>,
    /// Slider being dragged: `(id, position last sent)`.
    drag: Option<(&'static str, usize)>,
}

impl Ui {
    fn new(shared: Arc<Shared>, start: Start, cx: &mut Context<Self>) -> Self {
        let hw = shared.hw.clone();
        let latest = shared.clone();
        // The hardware is polled by a background thread; this only mirrors the latest snapshot. Err =
        // the view is gone, stop.
        cx.spawn(async move |this, cx| loop {
            let snap = latest.latest();
            if this.update(cx, |ui, cx| { ui.snap = snap; cx.notify(); }).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_millis(500)).await;
        })
        .detach();
        let mut settings = Settings::load();
        settings.light |= start.light;
        let font_scroll = UniformListScrollHandle::new();
        font_scroll.scroll_to_item(0, ScrollStrategy::Top);
        Self {
            hw,
            snap: shared.latest(),
            shared,
            tab: start.tab,
            fonts: settings::installed_fonts(),
            settings,
            system_font: settings::system_mono().into(),
            dropdown_open: start.open && start.tab == Tab::Profiles,
            font_open: start.open && start.tab == Tab::Settings,
            font_scroll,
            autostart: autostart::Layout::system().is_enabled(),
            preview: None,
            sliders: Rc::new(RefCell::new(HashMap::new())),
            drag: None,
        }
    }

    fn p(&self) -> Palette {
        if self.settings.light { Palette::light() } else { Palette::dark() }
    }

    fn family(&self) -> SharedString {
        self.settings.font.clone().map_or_else(|| self.system_font.clone(), Into::into)
    }

    /// Run a hardware action off the UI thread, then refresh the snapshot.
    fn act(&mut self, cx: &mut Context<Self>, job: impl FnOnce(&mut Hw) -> Result<String, String> + Send + 'static) {
        let hw = self.hw.clone();
        cx.spawn(async move |this, cx| {
            let snap = cx
                .background_executor()
                .spawn(async move {
                    let mut hw = hw.lock().unwrap_or_else(|e| e.into_inner());
                    hw.event = job(&mut hw).unwrap_or_else(|e| format!("error: {e}"));
                    hw.snapshot()
                })
                .await;
            this.update(cx, |ui, cx| ui.set_snap(snap, cx)).ok();
        })
        .detach();
    }

    /// The snapshot after a click: shown at once and published (popups) like the poller's.
    fn set_snap(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        self.shared.publish(snap.clone());
        self.snap = snap;
        cx.notify();
    }

    /// Switch the open-at-login entry; the result (or error) goes to the status bar.
    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let layout = autostart::Layout::system();
        let res = if layout.is_enabled() {
            layout.disable().map(|()| "open at login: off".to_string())
        } else {
            std::env::current_exe().and_then(|exe| layout.enable(&exe, self.settings.start_minimized)).map(|()| "open at login: on".to_string())
        };
        self.hw.lock().unwrap_or_else(|e| e.into_inner()).event = res.unwrap_or_else(|e| format!("error: open at login: {e}"));
        self.autostart = layout.is_enabled();
        cx.notify();
    }

    /// "Start minimized": saved, and the login script is rewritten when open-at-login is on.
    fn toggle_start_minimized(&mut self, cx: &mut Context<Self>) {
        self.settings.start_minimized = !self.settings.start_minimized;
        let layout = autostart::Layout::system();
        if layout.is_enabled() {
            let res = std::env::current_exe().and_then(|exe| layout.enable(&exe, self.settings.start_minimized));
            if let Err(e) = res {
                self.hw.lock().unwrap_or_else(|e| e.into_inner()).event = format!("error: open at login: {e}");
            }
        }
        self.save_settings(cx);
    }

    /// Persist UI preferences; failures show up in the status bar.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.settings.save() {
            self.hw.lock().unwrap_or_else(|e| e.into_inner()).event = format!("error: could not save settings: {e}");
        }
        cx.notify();
    }

    // ---- slider pointer handling

    fn step_for(&self, id: &'static str, x: f32) -> Option<(usize, usize, Pick)> {
        let reg = self.sliders.borrow();
        let r = reg.get(id)?;
        let b = r.bounds?;
        Some((step_at(x, f32::from(b.origin.x), f32::from(b.size.width), r.n), r.value, r.pick.clone()))
    }

    fn slider_press(&mut self, id: &'static str, x: f32, cx: &mut Context<Self>) {
        if let Some((i, value, pick)) = self.step_for(id, x) {
            self.drag = Some((id, i));
            if i != value {
                let job = pick(i);
                self.act(cx, job);
            }
            cx.notify();
        }
    }

    fn slider_drag(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((id, last)) = self.drag else { return };
        if let Some((i, _, pick)) = self.step_for(id, x) {
            if i != last {
                self.drag = Some((id, i));
                let job = pick(i);
                self.act(cx, job);
                cx.notify();
            }
        }
    }

    /// Send the final position once more: drag updates run as independent jobs, this guarantees
    /// the hardware ends on the value the thumb was released at.
    fn slider_release(&mut self, cx: &mut Context<Self>) {
        let Some((id, i)) = self.drag.take() else { return };
        let pick = self.sliders.borrow().get(id).map(|r| r.pick.clone());
        if let Some(pick) = pick {
            let job = pick(i);
            self.act(cx, job);
        }
    }

    /// A stepped slider: track (or coloured cells), fill, ticks, a thumb and optional labels.
    /// Click anywhere on it or drag; the thumb snaps to the nearest step.
    fn slider(&self, cx: &mut Context<Self>, spec: SliderSpec, pick: impl Fn(usize) -> Job + 'static) -> Stateful<Div> {
        let p = self.p();
        let id = spec.id;
        let n = spec.n.max(2);
        let prev = self.sliders.borrow().get(id).and_then(|r| r.bounds);
        self.sliders.borrow_mut().insert(id, SliderReg { bounds: prev, n, value: spec.value, pick: Rc::new(pick) });

        // while dragging show the thumb where the pointer is, not where the hardware last was
        let shown = match self.drag {
            Some((d, i)) if d == id => i,
            _ => spec.value,
        };
        let frac = |i: usize| i as f32 / (n - 1) as f32;
        let regs = self.sliders.clone();

        let mut inner = div().absolute().left(px(10.)).right(px(10.)).top_0().bottom_0().child(
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
                inner = inner.child(div().absolute().left_0().right_0().top(px(12.)).h(px(6.)).rounded_full().bg(rgb(p.border)));
                if spec.fill {
                    inner = inner.child(div().absolute().left_0().top(px(12.)).h(px(6.)).w(relative(frac(shown))).rounded_full().bg(rgb(p.accent)));
                }
                if n <= 24 {
                    for i in 0..n {
                        inner = inner.child(div().absolute().left(relative(frac(i))).top(px(24.)).w(px(1.)).h(px(4.)).bg(rgb(p.muted)));
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
                .border_color(rgb(p.accent))
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
                    .text_size(px(11.))
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
            .on_mouse_down(MouseButton::Left, cx.listener(move |ui, e: &MouseDownEvent, _, cx| ui.slider_press(id, f32::from(e.position.x), cx)))
            .child(inner)
    }
}

// ------------------------------------------------------------------ small widgets

fn button(p: Palette, id: impl Into<ElementId>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
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

fn section(p: Palette, title: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(rgb(p.panel))
        .border_1()
        .border_color(rgb(p.border))
        .child(div().text_color(rgb(p.muted)).child(title.to_uppercase()))
}

fn row(p: Palette, label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    div().flex().justify_between().gap_4().child(label.into()).child(div().text_color(rgb(p.muted)).child(value.into()))
}

fn note(p: Palette, text: impl Into<SharedString>) -> Div {
    div().text_color(rgb(p.muted)).text_size(px(12.)).child(text.into())
}

fn preset_of(s: &Snapshot, p: Profile) -> Preset {
    match p {
        Profile::Stock(m) => s.stock[m as usize],
        Profile::Custom => s.custom,
    }
}

/// Display colour (0xRRGGBB) of an HSV choice, via the EC's own level maths.
fn hsv_hex(h: u16, sat: u8, v: u8) -> u32 {
    let (r, g, b) = Hsv { h, s: sat, v }.to_levels();
    let f = |l: u8| u32::from(l) * 255 / u32::from(control::KB_LEVEL_MAX);
    (f(r) << 16) | (f(g) << 8) | f(b)
}

/// One line of the "what changes" panel: `PL1  35 W → 55 W` (or unchanged).
fn change(p: Palette, label: &str, from: Option<u8>, to: u8, prefix: &str) -> Div {
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

fn window_button(id: &'static str, glyph: Div, hover: u32) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(42.))
        .h(px(34.))
        .flex()
        .items_center()
        .justify_center()
        .hover(move |s| s.bg(rgb(hover)))
        // Without this the titlebar's drag handler would start a window move on the click.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(glyph)
}

fn resize_handles() -> Vec<Div> {
    let (e, c) = (px(5.), px(12.));
    let mk = |edge: ResizeEdge, d: Div| d.absolute().on_mouse_down(MouseButton::Left, move |_, w, _| w.start_window_resize(edge));
    vec![
        mk(ResizeEdge::Top, div().top_0().left(c).right(c).h(e).cursor_ns_resize()),
        mk(ResizeEdge::Bottom, div().bottom_0().left(c).right(c).h(e).cursor_ns_resize()),
        mk(ResizeEdge::Left, div().left_0().top(c).bottom(c).w(e).cursor_ew_resize()),
        mk(ResizeEdge::Right, div().right_0().top(c).bottom(c).w(e).cursor_ew_resize()),
        mk(ResizeEdge::TopLeft, div().top_0().left_0().size(c).cursor_nwse_resize()),
        mk(ResizeEdge::BottomRight, div().bottom_0().right_0().size(c).cursor_nwse_resize()),
        mk(ResizeEdge::TopRight, div().top_0().right_0().size(c).cursor_nesw_resize()),
        mk(ResizeEdge::BottomLeft, div().bottom_0().left_0().size(c).cursor_nesw_resize()),
    ]
}

impl Ui {
    fn titlebar(&self) -> impl IntoElement {
        let p = self.p();
        let model = if self.snap.model.is_empty() { "detecting…".to_string() } else { self.snap.model.clone() };
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
                    .child(div().size(px(10.)).rounded_sm().bg(rgb(p.accent)))
                    .child("Ripple Control Center")
                    .child(div().text_color(rgb(p.muted)).child(model)),
            )
            .child(
                div()
                    .flex()
                    .child(
                        window_button("w-min", div().w(px(10.)).h(px(1.)).bg(rgb(p.fg)), p.hover)
                            .on_click(|_, window, _| window.minimize_window()),
                    )
                    .child(
                        window_button("w-max", div().size(px(9.)).border_1().border_color(rgb(p.fg)), p.hover)
                            .on_click(|_, window, _| window.zoom_window()),
                    )
                    .child(
                        window_button("w-close", div().text_size(px(16.)).child("×"), p.red)
                            .on_click(|_, window, _| window.remove_window()),
                    ),
            )
    }

    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p();
        let tabs = [
            (Tab::Profiles, "Profiles"),
            (Tab::Fan, "Fan"),
            (Tab::Keyboard, "Keyboard"),
            (Tab::System, "System"),
            (Tab::Settings, "Settings"),
        ];
        div().flex().h(px(34.)).bg(rgb(p.panel)).border_b_1().border_color(rgb(p.border)).children(tabs.map(|(tab, label)| {
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
                .when(!active, |d| d.text_color(rgb(p.muted)).hover(|s| s.bg(rgb(p.hover))))
                .on_click(cx.listener(move |ui, _, _, cx| {
                    ui.tab = tab;
                    ui.dropdown_open = false;
                    ui.font_open = false;
                    cx.notify();
                }))
                .child(label)
        }))
    }

    fn status_bar(&self) -> impl IntoElement {
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
            .text_size(px(12.))
            .text_color(rgb(p.muted))
            .child(div().truncate().child(left))
            .child(div().truncate().child(s.event.clone()))
            .child(div().flex_shrink_0().text_color(rgb(if s.button { p.green } else { p.muted })).child(format!("{dot} {label}")))
    }

    // -------------------------------------------------------------- tabs

    /// Select-style popover: trigger and list share one width, columns line up, the list floats
    /// over the page (it never pushes content), and a click anywhere else closes it.
    fn profile_dropdown(&self, cx: &mut Context<Self>) -> Div {
        const W: f32 = 400.;
        const NAME_W: f32 = 76.;
        const CPU_W: f32 = 132.;
        let p = self.p();
        let s = &self.snap;
        let open = self.dropdown_open;
        let current = s.profile.map_or("Unknown", Profile::name);

        let dot = move |on: bool| div().size(px(8.)).rounded_full().when(on, |d| d.bg(rgb(p.accent)));
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
            .on_mouse_down(MouseButton::Left, cx.listener(move |ui, _, _, cx| {
                ui.dropdown_open = !open;
                ui.preview = None;
                cx.notify();
            }))
            .child(div().flex().items_center().gap_2().child(dot(s.profile.is_some())).child(current))
            .child(div().text_color(rgb(p.muted)).child(if open { "▴" } else { "▾" }));

        let mut wrap = div().relative().w(px(W)).child(trigger);
        if open {
            let header = div()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .h(px(24.))
                .text_size(px(11.))
                .text_color(rgb(p.muted))
                .child(div().w(px(8.)))
                .child(div().w(px(NAME_W)).child("PROFILE"))
                .child(div().w(px(CPU_W)).child("CPU PL1 / PL2"))
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
            for (i, m) in PROFILES.into_iter().enumerate() {
                let pr = preset_of(s, m);
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
                        .child(div().w(px(NAME_W)).child(m.name()))
                        .child(div().w(px(CPU_W)).when(!active, |d| d.text_color(rgb(p.muted))).child(format!("{} / {} W", pr.pl1, pr.pl2)))
                        .child(div().when(!active, |d| d.text_color(rgb(p.muted))).child(format!("+{} W", pr.ctgp))),
                );
            }
            // `deferred` paints the list after everything else, so later sections cannot cover it.
            wrap = wrap.child(deferred(list).with_priority(1));
        }
        wrap
    }

    /// A watt slider (5 W steps) for one Custom-preset value.
    fn custom_slider(&self, cx: &mut Context<Self>, id: &'static str, field: Field, value: u8, lo: u8, hi: u8) -> Stateful<Div> {
        let steps: Vec<u8> = (lo..=hi).step_by(5).collect();
        let sel = steps.iter().position(|v| *v >= value).unwrap_or(steps.len() - 1);
        let labels = vec![(0, lo.to_string()), (steps.len() - 1, hi.to_string())];
        self.slider(cx, SliderSpec { id, n: steps.len(), value: sel, fill: true, labels, cells: None }, move |i| {
            let v = steps[i];
            Box::new(move |hw| hw.set_custom(field, v))
        })
    }

    fn profiles_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        let target = self.preview.or(s.profile);

        // What the hovered (or, when closed, the active) profile sets.
        let mut changes = section(
            p,
            &match target {
                Some(m) if Some(m) != s.profile => format!("Switch to {}", m.name()),
                Some(m) => m.name().to_string(),
                None => "Current limits".to_string(),
            },
        );
        match target {
            Some(m) => {
                let pr = preset_of(s, m);
                changes = changes
                    .child(change(p, "CPU PL1", s.pl.map(|v| v.0), pr.pl1, ""))
                    .child(change(p, "CPU PL2", s.pl.map(|v| v.1), pr.pl2, ""))
                    .child(change(p, "GPU cTGP", s.ctgp, pr.ctgp, "+"));
                match m {
                    Profile::Stock(OperatingMode::Turbo) => {
                        changes = changes.child(note(p, "The stock app's values for this machine. Runs hot under load; the GPU part needs the NVIDIA driver."));
                    }
                    Profile::Custom => changes = changes.child(note(p, "Your own preset, edited below.")),
                    Profile::Stock(_) => {}
                }
            }
            None => {
                if let Some((a, b)) = s.pl {
                    changes = changes.child(row(p, "CPU PL1 / PL2", format!("{a} / {b} W")));
                }
            }
        }

        // The only editable preset; the stock three are fixed.
        let c = s.custom;
        let custom = section(p, "Custom performance profile preset")
            .child(
                div().flex().gap_2().child(
                    button(p, "custom-default", "Default preset", false).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.reset_custom()))),
                ),
            )
            .child(row(p, "CPU PL1", format!("{} W", c.pl1)))
            .child(self.custom_slider(cx, "custom-pl1", Field::Pl1, c.pl1, 5, control::PL_CEILING_W))
            .child(row(p, "CPU PL2", format!("{} W", c.pl2)))
            .child(self.custom_slider(cx, "custom-pl2", Field::Pl2, c.pl2, 5, control::PL_CEILING_W))
            .child(row(p, "GPU cTGP", format!("+{} W", c.ctgp)))
            .child(self.custom_slider(cx, "custom-ctgp", Field::Ctgp, c.ctgp, 0, control::CTGP_CEILING_W))
            .child(note(p, format!("Balanced, Enthusiast and Overboost are fixed. Edit this one, then pick Custom. Capped at {} W (cTGP {} W).", control::PL_CEILING_W, control::CTGP_CEILING_W)));

        let mut page = div().flex().flex_col().gap_3();
        if let Some(e) = &s.ec_error {
            page = page.child(div().p_3().rounded_md().bg(rgb(p.err_bg)).text_color(rgb(p.red)).child(format!("EC unavailable: {e}")));
        }
        let profile = section(p, "Profile")
            .child(self.profile_dropdown(cx))
            .child(note(p, "The mode button cycles Balanced → Enthusiast → Overboost; lights 0 / 1 / 2 (Custom shows 1)."));
        page.child(profile).child(changes).child(custom).into_any_element()
    }

    fn fan_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;

        let current = if s.fan.enabled {
            FanMode::Custom
        } else if s.boosted {
            FanMode::Boost
        } else {
            FanMode::Auto
        };
        let mode_button = |cx: &mut Context<Self>, id: &'static str, label: &'static str, m: FanMode| {
            button(p, id, label, current == m).on_click(cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_fan_mode(m))))
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
            .child(note(p, "Auto: the firmware's own curve. Custom: your curves below, any time. Boost: both fans at full speed."));

        let mut fans = section(p, "Sensors");
        if s.temps.is_empty() && s.fans.is_empty() {
            fans = fans.child(note(p, "No hwmon sensors (kernel driver not loaded?)."));
        }
        for (l, c) in &s.temps {
            fans = fans.child(row(p, l.clone(), format!("{c:.1} °C")));
        }
        for (l, rpm, duty) in &s.fans {
            fans = fans.child(row(p, l.clone(), format!("{rpm} rpm{}", duty.map(|d| format!("  ({d} %)")).unwrap_or_default())));
        }

        // Off, then 60..=95 °C in 5 °C steps.
        let steps: Vec<Option<u8>> = std::iter::once(None).chain((60..=95).step_by(5).map(Some)).collect();
        let sel = steps.iter().position(|t| *t == s.auto_boost).unwrap_or(0);
        let labels = steps.iter().enumerate().map(|(i, t)| (i, t.map_or("Off".to_string(), |v| v.to_string()))).collect();
        let slider = self.slider(
            cx,
            SliderSpec { id: "auto-boost", n: steps.len(), value: sel, fill: sel > 0, labels, cells: None },
            move |i| {
                let t = steps[i];
                Box::new(move |hw| hw.set_auto_boost(t))
            },
        );
        let auto = section(p, "Auto-boost")
            .child(row(p, "Boost above", s.auto_boost.map_or("off".to_string(), |t| format!("{t} °C"))))
            .child(slider)
            .child(note(p, "Boost on at the threshold, off 8 °C lower. Runs while this app is open."));

        let mut profile = section(p, "Custom fan profile")
            .child(
                div().flex().gap_2().child(
                    button(p, "fan-default", "Default preset", false).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.reset_fan_curves()))),
                ),
            )
            .child(note(
                p,
                format!("Duty at each temperature, linear in between. Fan 1 follows the CPU temperature, fan 2 the GPU's; both run at 100 % from {} °C or without a sensor.", control::FAN_PANIC_C),
            ));
        if s.fan.enabled && !s.service {
            profile = profile.child(
                div().p_3().rounded_md().bg(rgb(p.err_bg)).text_color(rgb(p.red)).child("The curves are applied by the ripple-nova service, which is not running (./install.sh)."),
            );
        }
        let cpu = self.fan_curve(cx, "Fan 1 (CPU) curve", 0, s.fan.cpu);
        let gpu = self.fan_curve(cx, "Fan 2 (GPU) curve", 1, s.fan.gpu);

        div().flex().flex_col().gap_3().child(mode).child(fans).child(profile).child(cpu).child(gpu).child(auto).into_any_element()
    }

    /// Five duty sliders, one per anchor temperature, for fan 0 (CPU) or 1 (GPU).
    fn fan_curve(&self, cx: &mut Context<Self>, title: &str, fan: usize, duty: [u8; 5]) -> Div {
        const IDS: [[&str; 5]; 2] = [
            ["fan-cpu-0", "fan-cpu-1", "fan-cpu-2", "fan-cpu-3", "fan-cpu-4"],
            ["fan-gpu-0", "fan-gpu-1", "fan-gpu-2", "fan-gpu-3", "fan-gpu-4"],
        ];
        let p = self.p();
        let mut sec = section(p, title);
        for (i, d) in duty.into_iter().enumerate() {
            let labels = if i == 4 { vec![(0, "0".into()), (10, "50".into()), (20, "100".into())] } else { vec![] };
            sec = sec.child(row(p, format!("{} °C", control::FAN_ANCHORS[i]), format!("{d} %"))).child(self.slider(
                cx,
                SliderSpec { id: IDS[fan][i], n: 21, value: usize::from(d) / 5, fill: true, labels, cells: None },
                move |k| Box::new(move |hw| hw.set_fan_point(fan, i, (k * 5) as u8)),
            ));
        }
        sec
    }

    fn keyboard_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        if !s.has_keyboard {
            return section(p, "Keyboard backlight: unavailable").child(note(p, "No RGB keyboard reported by the EC.")).into_any_element();
        }
        let hsv = s.hsv;
        let full = hsv_hex(hsv.h, hsv.s, 100);

        // Presets set hue and saturation; the brightness you chose is kept.
        let colours: [(u16, u8); 9] = [(0, 100), (30, 100), (60, 100), (120, 100), (180, 100), (240, 100), (270, 80), (300, 100), (0, 0)];
        let swatches = div().flex().flex_wrap().gap_2().children(colours.iter().enumerate().map(|(i, (h, sat))| {
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
                    hw.set_keyboard(Hsv { h: i as u16 * 10, s: if cur.s == 0 { 100 } else { cur.s }, v: cur.v })
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
                labels: vec![(0, "0%".into()), (10, "100%".into())],
                cells: Some((0..=10).map(|i| hsv_hex(hsv.h, i * 10, 100)).collect()),
            },
            |i| {
                Box::new(move |hw| {
                    let cur = hw.hsv();
                    hw.set_keyboard(Hsv { s: i as u8 * 10, ..cur })
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
                    hw.set_keyboard(Hsv { v: i as u8 * 5, ..cur })
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
                            .child(div().size_8().rounded_md().bg(rgb(full)).border_1().border_color(rgb(p.border)))
                            .child(format!("#{full:06x}")),
                    )
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

    fn system_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        fn describe(name: &str) -> &str {
            match name {
                "fn_lock" => "F-keys act as media keys unless Fn is held",
                "super_key_enable" => "The Super (Windows) key works",
                "ac_auto_boot" => "Power on when AC is plugged in",
                "usb_powershare_high" => "Keep the USB ports powered when off",
                "touchpad" => "Off powers the touchpad down: no pointer, its LED lights (as in suspend)",
                "breathing_in_suspend" => "Breathing LED animation while suspended on AC power (a lightbar feature)",
                "touchpad_toggle_enable" => "The firmware's own touchpad toggle; no effect seen on this laptop",
                _ => "",
            }
        }
        let mut toggles = section(p, "Toggles");
        let popups = self.settings.popups;
        toggles = toggles.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap_4()
                .child(div().child("Show popup").child(note(p, "Notify when a toggle, the performance profile or fan boost changes.")))
                .child(button(p, "popup-toggle", if popups { "On" } else { "Off" }, popups).on_click(cx.listener(|ui, _, _, cx| {
                    ui.settings.popups = !ui.settings.popups;
                    ui.save_settings(cx);
                }))),
        );
        if s.toggles.is_empty() {
            toggles = toggles.child(note(p, "none"));
        }
        for (i, (name, on)) in s.toggles.iter().copied().enumerate() {
            toggles = toggles.child(
                div().flex().justify_between().items_center().gap_4().child(
                    div()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(toggle_label(name))
                                .when(is_experimental(name), |d| d.child(div().text_size(px(11.)).text_color(rgb(p.muted)).child("experimental"))),
                        )
                        .child(note(p, describe(name))),
                ).child(
                    button(p, ("toggle", i), if on { "On" } else { "Off" }, on)
                        .on_click(cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_toggle(name, !on)))),
                ),
            );
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
                    .on_click(cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_lightbar(Some(c)))))
            };
            let colours = [0xffffff, 0xff0000, 0xff8000, 0xffff00, 0x00ff00, 0x00ffff, 0x0000ff, 0xff00ff];
            page = page.child(
                section(p, "Lightbar")
                    .child(div().flex().flex_wrap().gap_2().children(colours.iter().enumerate().map(|(i, h)| swatch(i, *h))))
                    .child(button(p, "lb-off", "Off", false).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_lightbar(None))))),
            );
        } else {
            page = page.child(note(p, "No lightbar on this laptop (the EC reports none), so its controls are hidden."));
        }
        page.into_any_element()
    }

    /// Font select: same popover pattern as the profile dropdown. Every installed family is listed
    /// (virtualized, so hundreds of entries stay fast) and drawn in its own font. The first entry
    /// is the default, the system monospace font.
    fn font_dropdown(&self, cx: &mut Context<Self>) -> Div {
        const W: f32 = 400.;
        let p = self.p();
        let open = self.font_open;
        let current = self.settings.font.clone().unwrap_or_else(|| format!("Monospace (system: {})", self.system_font));

        let trigger = div()
            .id("font-dropdown")
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
            .on_mouse_down(MouseButton::Left, cx.listener(move |ui, _, _, cx| {
                ui.font_open = !open;
                if !open {
                    let sel = ui.settings.font.as_ref().and_then(|f| ui.fonts.iter().position(|n| n == f)).map_or(0, |i| i + 1);
                    ui.font_scroll.scroll_to_item(sel, ScrollStrategy::Top);
                }
                cx.notify();
            }))
            .child(div().truncate().font_family(self.family()).child(current))
            .child(div().text_color(rgb(p.muted)).child(if open { "▴" } else { "▾" }));

        let mut wrap = div().relative().w(px(W)).child(trigger);
        if open {
            let rows = uniform_list(
                "font-rows",
                self.fonts.len() + 1,
                cx.processor(|ui, range: Range<usize>, _window, cx| {
                    let p = ui.p();
                    range
                        .map(|ix| {
                            let (label, family, pick, active) = if ix == 0 {
                                (format!("Monospace (system: {})", ui.system_font), ui.system_font.clone(), None, ui.settings.font.is_none())
                            } else {
                                let n = ui.fonts[ix - 1].clone();
                                let active = ui.settings.font.as_deref() == Some(n.as_str());
                                (n.clone(), SharedString::from(n.clone()), Some(n), active)
                            };
                            div()
                                .id(("font", ix))
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(move |st| st.bg(rgb(p.hover)))
                                .when(active, |d| d.text_color(rgb(p.accent)))
                                .on_click(cx.listener(move |ui, _, _, cx| {
                                    ui.settings.font = pick.clone();
                                    ui.font_open = false;
                                    ui.save_settings(cx);
                                }))
                                .child(div().truncate().font_family(family).child(label))
                                .child(div().text_color(rgb(p.accent)).child(if active { "●" } else { "" }))
                        })
                        .collect()
                }),
            )
            .track_scroll(self.font_scroll.clone())
            .h(px(280.));
            let list = div()
                .absolute()
                .top(px(36.))
                .left_0()
                .w(px(W))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(p.border))
                .bg(rgb(p.popup))
                .shadow_lg()
                .occlude()
                .on_mouse_down_out(cx.listener(|ui, _, _, cx| {
                    ui.font_open = false;
                    cx.notify();
                }))
                .child(rows);
            wrap = wrap.child(deferred(list).with_priority(1));
        }
        wrap
    }

    fn settings_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let light = self.settings.light;

        let theme = section(p, "Theme").child(
            div()
                .flex()
                .gap_2()
                .child(button(p, "theme-dark", "Dark", !light).on_click(cx.listener(|ui, _, _, cx| {
                    ui.settings.light = false;
                    ui.save_settings(cx);
                })))
                .child(button(p, "theme-light", "Light", light).on_click(cx.listener(|ui, _, _, cx| {
                    ui.settings.light = true;
                    ui.save_settings(cx);
                }))),
        );

        let font = section(p, "Font")
            .child(self.font_dropdown(cx))
            .child(div().font_family(self.family()).child("The quick brown fox 0123456789"))
            .child(note(p, format!("{} families. Default: the system monospace font.", self.fonts.len())));

        let startup = section(p, "Startup")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(button(p, "autostart", if self.autostart { "On" } else { "Off" }, self.autostart).on_click(cx.listener(|ui, _, _, cx| ui.toggle_autostart(cx))))
                    .child("Open at login"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(button(p, "start-min", if self.settings.start_minimized { "On" } else { "Off" }, self.settings.start_minimized).on_click(cx.listener(|ui, _, _, cx| ui.toggle_start_minimized(cx))))
                    .child("Start minimized"),
            )
            .child(note(p, "Open at login starts the window at login (no password: the sudoers rule from install.sh, else polkit asks). Start minimized starts it minimized, out of the way until you open it (dash, or the tray icon)."));

        div().flex().flex_col().gap_3().child(theme).child(font).child(startup).into_any_element()
    }
}

impl Render for Ui {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p();
        let body = match self.tab {
            Tab::Profiles => self.profiles_tab(cx),
            Tab::Fan => self.fan_tab(cx),
            Tab::Keyboard => self.keyboard_tab(cx),
            Tab::System => self.system_tab(cx),
            Tab::Settings => self.settings_tab(cx),
        };
        div()
            .id("window")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.bg))
            .text_color(rgb(p.fg))
            .font_family(self.family())
            .text_size(px(13.))
            .border_1()
            .border_color(rgb(p.border))
            // slider drags continue outside the slider, anywhere in the window
            .on_mouse_move(cx.listener(|ui, e: &MouseMoveEvent, _, cx| {
                if ui.drag.is_some() {
                    if e.pressed_button == Some(MouseButton::Left) {
                        ui.slider_drag(f32::from(e.position.x), cx);
                    } else {
                        ui.slider_release(cx);
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|ui, _, _, cx| ui.slider_release(cx)))
            .child(self.titlebar())
            .child(self.tab_bar(cx))
            .child(div().id("content").flex_1().min_h_0().overflow_y_scroll().p_4().child(body))
            .child(self.status_bar())
            .children(resize_handles())
    }
}

// ------------------------------------------------------------------ startup

/// Where `rcc-tray` lives: next to this binary (installed, or a build directory), else the usual places.
fn tray_binary() -> std::path::PathBuf {
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("rcc-tray")));
    beside
        .into_iter()
        .chain(["/usr/local/bin/rcc-tray", "/usr/bin/rcc-tray"].map(std::path::PathBuf::from))
        .find(|p| p.exists())
        .unwrap_or_else(|| "/usr/local/bin/rcc-tray".into())
}

/// Held for as long as the app runs; `/run/ripple-nova-gui.lock` is also what the launcher looks at.
/// `Err` = another instance holds it. A lock file that cannot even be opened (not root: the smoke
/// tests) does not stop anything.
fn single_instance() -> Result<Option<std::fs::File>, ()> {
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(ripple_hw::notify::GUI_LOCK) else {
        return Ok(None);
    };
    match f.try_lock() {
        Err(std::fs::TryLockError::WouldBlock) => Err(()),
        _ => Ok(Some(f)),
    }
}

/// Follow the hardware mode button (WMI event 176 = key 184): each press cycles the profile,
/// exactly what the stock Windows service does. Only one process may follow it: when the systemd
/// service already holds the lock, the app leaves the button to it.
fn spawn_mode_button(hw: Arc<Mutex<Hw>>) {
    std::thread::spawn(move || {
        let Some(_lock) = control::follow_lock(false) else {
            let mut hw = hw.lock().unwrap_or_else(|e| e.into_inner());
            hw.button = true;
            hw.service = true;
            return;
        };
        let mut dev = match control::open_mode_key() {
            Ok(d) => d,
            Err(e) => {
                hw.lock().unwrap_or_else(|e| e.into_inner()).event = format!("mode button: {e}");
                return;
            }
        };
        hw.lock().unwrap_or_else(|e| e.into_inner()).button = true;
        while control::wait_mode_key(&mut dev).is_ok() {
            let mut hw = hw.lock().unwrap_or_else(|e| e.into_inner());
            hw.event = hw.cycle_profile();
        }
        hw.lock().unwrap_or_else(|e| e.into_inner()).button = false;
    });
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--show` / `--quit` talk to the running window as the desktop user, so they come before the root gate
    // (the launcher and the tray use them).
    if let Some(verb) = args.iter().find_map(|a| match a.as_str() {
        "--show" => Some("show"),
        "--quit" => Some("quit"),
        _ => None,
    }) {
        match remote::ask(verb) {
            Ok(()) => return,
            Err(e) => {
                eprintln!("Ripple Control Center is not running ({e})");
                std::process::exit(1);
            }
        }
    }
    if let Err(e) = ripple_hw::require_root(true) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
    if let Some(cmd) = args.iter().position(|a| a == "--autostart").and_then(|i| args.get(i + 1)) {
        let layout = autostart::Layout::system();
        let res = match cmd.as_str() {
            "on" => std::env::current_exe().and_then(|exe| layout.enable(&exe, Settings::load().start_minimized)),
            "off" => layout.disable(),
            "status" => Ok(()),
            other => {
                eprintln!("error: --autostart takes on|off|status, got '{other}'");
                std::process::exit(2);
            }
        };
        match res {
            Ok(()) => println!("open at login: {}", if layout.is_enabled() { "on" } else { "off" }),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // One window per machine: a second root GUI would follow the button twice and double every popup.
    // (The launcher checks this lock too, before it asks for anything.)
    let _instance = match single_instance() {
        Ok(lock) => lock,
        Err(()) => {
            eprintln!("Ripple Control Center is already running");
            return;
        }
    };
    // The tray icon belongs to the window's life: closing the window leaves the icon, the icon opens the window.
    // It runs in the user's session, so a root window starts it as that user (nothing happens if it runs).
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        ensure_tray(std::path::Path::new(&dir), &tray_binary());
    }
    let hw = Arc::new(Mutex::new(Hw::open()));
    spawn_mode_button(hw.clone());
    let shared = Arc::new(Shared {
        hw: hw.clone(),
        snap: Mutex::new(Snapshot::default()),
    });
    // The control socket (after the instance lock, so a stale one is ours to replace).
    let commands = remote::listen().map_err(|e| eprintln!("warning: control socket: {e}")).ok();
    // The poller (also runs the auto-boost policy) lives as long as the process, window or not.
    let poller = shared.clone();
    std::thread::spawn(move || loop {
        let snap = poller.hw.lock().unwrap_or_else(|e| e.into_inner()).poll();
        poller.publish(snap);
        std::thread::sleep(Duration::from_secs(1));
    });
    let start = Start::from_args();
    Application::new().run(move |cx: &mut App| {
        // gpui stops its event loop when the last window closes (measured on Wayland), so closing the window
        // ends the process: the popups live in the daemon, the tray icon in its own helper.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        open_main_window(cx, shared.clone(), start); // --minimized: the window starts minimized
        cx.activate(true);
        // Whoever lives in the user's session (the tray, the launcher) can ask the window to come forward or close.
        if let Some(commands) = commands {
            let shared = shared.clone();
            cx.spawn(async move |cx| loop {
                while let Ok(cmd) = commands.try_recv() {
                    let shared = shared.clone();
                    let alive = cx.update(|cx| match cmd {
                        remote::Cmd::Quit => cx.quit(),
                        remote::Cmd::Show => show_window(cx, shared),
                    });
                    if alive.is_err() {
                        return; // the app is gone
                    }
                }
                cx.background_executor().timer(Duration::from_millis(150)).await;
            })
            .detach();
        }
    });
}

fn open_main_window(cx: &mut App, shared: Arc<Shared>, start: Start) {
    let minimized = start.minimized;
    let (w, h) = start.size.unwrap_or((860., 640.));
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Ripple Control Center".into()),
                appears_transparent: true,
                traffic_light_position: None,
            }),
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(size(px(640.), px(460.))),
            app_id: Some("ripple-nova".into()),
            ..Default::default()
        },
        move |window, cx| {
            if minimized {
                window.minimize_window();
            }
            cx.new(|cx| Ui::new(shared, start, cx))
        },
    )
    .unwrap();
}

/// `show` (the tray click, the menu launcher): get a usable window in front of the user. Measured on native
/// Wayland: `activate_window` on a *minimized* window does nothing (GNOME only posts a "... is ready"
/// notice) and a client cannot un-minimize itself. So a window that is not the active one is replaced by a
/// fresh one. The new window opens first: gpui stops the whole event loop when the window count hits zero.
fn show_window(cx: &mut App, shared: Arc<Shared>) {
    let old = cx.windows();
    let active = old.iter().any(|w| w.update(cx, |_, window, _| window.is_window_active()).unwrap_or(false));
    if active {
        return;
    }
    open_main_window(cx, shared, Start::plain());
    for w in old {
        let _ = w.update(cx, |_, window, _| window.remove_window());
    }
    cx.activate(true);
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
}
