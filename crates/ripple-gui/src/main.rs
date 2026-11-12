//! `rcc-gui` — Ripple Control Center GPUI app. Root only (EC access).
//!
//! Zed-style window: client-side decorations (own titlebar, no GNOME bar), tabs, status bar,
//! One Dark / One Light palettes, a user-selectable font (system monospace by default). All
//! hardware I/O runs on GPUI's background executor behind one mutex, so the render path
//! never blocks.

mod autostart;
mod hw;
mod settings;

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    deferred, div, prelude::*, px, rgb, size, AnyElement, App, Application, Bounds, Context, Div, ElementId,
    MouseButton, ResizeEdge, SharedString, Stateful, TitlebarOptions, Window, WindowBounds, WindowDecorations,
    WindowOptions,
};
use hw::{Field, Hw, Snapshot, PROFILES};
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
/// `rcc-gui --autostart on|off|status` installs/removes/shows the start-at-login entry and exits.
struct Start {
    tab: Tab,
    open: bool,
    light: bool,
    /// `--size 860x1000`: window size for this run (screenshots of whole tabs)
    size: Option<(f32, f32)>,
    /// `--minimized`: start minimized (what the autostart entry uses)
    minimized: bool,
}

impl Start {
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
        Self { tab, open: args.iter().any(|a| a == "--open"), light: args.iter().any(|a| a == "--light"), size, minimized: args.iter().any(|a| a == "--minimized") }
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

struct Ui {
    hw: Arc<Mutex<Hw>>,
    snap: Snapshot,
    tab: Tab,
    settings: Settings,
    system_font: SharedString,
    fonts: Vec<String>,
    dropdown_open: bool,
    font_open: bool,
    /// Is the start-at-login entry installed?
    autostart: bool,
    /// Profile under the mouse in the dropdown, shown in the "what changes" panel.
    preview: Option<Profile>,
}

impl Ui {
    fn new(hw: Arc<Mutex<Hw>>, start: Start, cx: &mut Context<Self>) -> Self {
        let poll = hw.clone();
        cx.spawn(async move |this, cx| loop {
            let hw = poll.clone();
            let snap = cx
                .background_executor()
                .spawn(async move { hw.lock().unwrap_or_else(|e| e.into_inner()).poll() })
                .await;
            // Err = the view is gone, stop polling.
            if this.update(cx, |ui, cx| { ui.snap = snap; cx.notify(); }).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(1)).await;
        })
        .detach();
        let mut settings = Settings::load();
        settings.light |= start.light;
        Self {
            hw,
            snap: Snapshot::default(),
            tab: start.tab,
            fonts: settings::installed_fonts(settings.all_fonts),
            settings,
            system_font: settings::system_mono().into(),
            dropdown_open: start.open && start.tab == Tab::Profiles,
            font_open: start.open && start.tab == Tab::Settings,
            autostart: autostart::Layout::system().is_enabled(),
            preview: None,
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
            this.update(cx, |ui, cx| {
                ui.snap = snap;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switch the start-at-login entry; the result (or error) goes to the status bar.
    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let layout = autostart::Layout::system();
        let res = if layout.is_enabled() {
            layout.disable().map(|()| "start at login: off".to_string())
        } else {
            std::env::current_exe()
                .and_then(|exe| layout.enable(&exe))
                .map(|()| format!("start at login: on (installed {})", layout.dir.display()))
        };
        self.hw.lock().unwrap_or_else(|e| e.into_inner()).event = res.unwrap_or_else(|e| format!("error: start at login: {e}"));
        self.autostart = layout.is_enabled();
        cx.notify();
    }

    /// Persist UI preferences; failures show up in the status bar.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.settings.save() {
            self.hw.lock().unwrap_or_else(|e| e.into_inner()).event = format!("error: could not save settings: {e}");
        }
        cx.notify();
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

/// One line of the "what changes" panel: `PL1  35 W → 55 W` (or "unchanged").
fn change(p: Palette, label: &str, from: Option<u8>, to: u8, prefix: &str) -> Div {
    let to_s = format!("{prefix}{to} W");
    let value = match from {
        Some(f) if f == to => format!("{prefix}{f} W (unchanged)"),
        Some(f) => format!("{prefix}{f} W → {to_s}"),
        None => to_s,
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
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(if s.button { p.green } else { p.muted }))
                    .child(if s.button { "● mode button followed" } else { "○ mode button not followed" }),
            )
    }

    /// A row of clickable cells acting as a slider. `lit_from_start` fills the bar up to the
    /// selection (levels); otherwise only the selected cell is marked (threshold with an "Off").
    fn stepped(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        labels: Vec<String>,
        sel: usize,
        lit_from_start: bool,
        pick: impl Fn(usize) -> Job + 'static,
    ) -> Div {
        let p = self.p();
        let pick = Rc::new(pick);
        let mut bar = div().flex().gap_1();
        for (i, label) in labels.into_iter().enumerate() {
            let pick = pick.clone();
            let lit = if lit_from_start { i <= sel } else { i <= sel && sel > 0 };
            let colour = if lit { p.accent } else if i == sel { p.muted } else { p.border };
            bar = bar.child(
                div()
                    .id((id, i))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(cx.listener(move |ui, _, _, cx| {
                        let job = pick(i);
                        ui.act(cx, job);
                    }))
                    .child(div().h(px(10.)).w_full().rounded_sm().bg(rgb(colour)))
                    .child(div().text_size(px(11.)).text_color(rgb(if i == sel { p.fg } else { p.muted })).child(label)),
            );
        }
        bar
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

    fn profiles_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        let target = self.preview.or(s.profile);

        // "What changes" for the hovered (or, when closed, the active) profile.
        let mut changes = section(
            p,
            &match target {
                Some(m) if Some(m) != s.profile => format!("Switching to {} changes", m.name()),
                Some(m) => format!("{} sets", m.name()),
                None => "Current limits".to_string(),
            },
        );
        match target {
            Some(m) => {
                let pr = preset_of(s, m);
                changes = changes
                    .child(change(p, "CPU sustained power (PL1)", s.pl.map(|v| v.0), pr.pl1, ""))
                    .child(change(p, "CPU boost power (PL2)", s.pl.map(|v| v.1), pr.pl2, ""))
                    .child(change(p, "GPU configurable TGP (cTGP)", s.ctgp, pr.ctgp, "+"))
                    .child(row(p, "Fan profile", "follows the mode (EC mode byte); boost is separate"));
                if m == Profile::Stock(OperatingMode::Turbo) {
                    changes = changes.child(note(p, "Turbo values are the stock app's for this machine (80 W, cTGP +20 W). It runs hot: expect throttling near 100 °C under all-core load. The GPU part only takes effect with the NVIDIA proprietary driver (this system uses nouveau)."));
                }
                if m == Profile::Custom {
                    changes = changes.child(note(p, "Your own preset: edit it in the Custom preset section below."));
                }
            }
            None => {
                if let Some((a, b)) = s.pl {
                    changes = changes.child(row(p, "PL1 / PL2", format!("{a} / {b} W (matches no profile)")));
                }
            }
        }

        // The only editable preset. Stock Office/Gaming/Turbo are fixed; anything custom lives here.
        let c = s.custom;
        let stepper = |field: Field, label: &'static str, value: u8, prefix: &'static str, base: usize| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(40.)).text_color(rgb(p.muted)).child(label))
                .child(
                    button(p, ("dec", base), "−", false)
                        .on_click(cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.adjust_custom(field, -5)))),
                )
                .child(div().w(px(64.)).flex().justify_center().child(format!("{prefix}{value} W")))
                .child(
                    button(p, ("inc", base), "+", false)
                        .on_click(cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.adjust_custom(field, 5)))),
                )
        };
        let custom = section(p, "Custom preset")
            .child(div().flex().flex_wrap().gap_x_6().gap_y_2()
                .child(stepper(Field::Pl1, "PL1", c.pl1, "", 0))
                .child(stepper(Field::Pl2, "PL2", c.pl2, "", 1))
                .child(stepper(Field::Ctgp, "cTGP", c.ctgp, "+", 2)))
            .child(note(p, format!(
                "Office, Gaming and Turbo are the stock presets and can't be edited. Change this one instead, then pick Custom in the dropdown to use it (the hardware button only cycles the stock three). Limits are capped at {} W (cTGP {} W); saved in {}.",
                control::PL_CEILING_W,
                control::CTGP_CEILING_W,
                control::Config::PATH
            )));

        let mut page = div().flex().flex_col().gap_3();
        if let Some(e) = &s.ec_error {
            page = page.child(div().p_3().rounded_md().bg(rgb(p.err_bg)).text_color(rgb(p.red)).child(format!("EC unavailable, profiles disabled: {e}")));
        }
        page.child(section(p, "Profile").child(self.profile_dropdown(cx)).child(note(
            p,
            "The hardware mode button cycles Office → Gaming → Turbo while this app (or `rcc watch`) is running, and its lights follow the profile: 0 lights Office, 1 Gaming, 2 Turbo (Custom shows as 1).",
        )))
        .child(changes)
        .child(custom)
        .into_any_element()
    }

    fn fan_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;

        let mode = section(p, "Fan mode")
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button(p, "fan-auto", "Auto", !s.boosted).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_boost(false)))))
                    .child(button(p, "fan-boost", "Boost", s.boosted).on_click(cx.listener(|ui, _, _, cx| ui.act(cx, |hw| hw.set_boost(true))))),
            )
            .child(note(p, "Auto: the firmware's own curve. Boost: both fans at full speed."));

        let mut fans = section(p, "Fans");
        if s.temps.is_empty() && s.fans.is_empty() {
            fans = fans.child(note(p, "no uniwill hwmon (kernel driver not loaded?)"));
        }
        for (l, c) in &s.temps {
            fans = fans.child(row(p, l.clone(), format!("{c:.1} °C")));
        }
        for (l, rpm, duty) in &s.fans {
            fans = fans.child(row(p, l.clone(), format!("{rpm} rpm{}", duty.map(|d| format!("  ({d} %)")).unwrap_or_default())));
        }

        // Stepped slider: Off, then 60..=95 °C in 5 °C steps.
        let steps: Vec<Option<u8>> = std::iter::once(None).chain((60..=95).step_by(5).map(Some)).collect();
        let sel = steps.iter().position(|t| *t == s.auto_boost).unwrap_or(0);
        let labels = steps.iter().map(|t| t.map_or("Off".to_string(), |v| v.to_string())).collect();
        let slider = self.stepped(cx, "ab", labels, sel, false, move |i| {
            let t = steps[i];
            Box::new(move |hw| hw.set_auto_boost(t))
        });
        let auto = section(p, "Auto-boost")
            .child(row(p, "Boost the fans above", s.auto_boost.map_or("off".to_string(), |t| format!("{t} °C"))))
            .child(slider)
            .child(note(p, "Switches Fan boost on when the hottest sensor reaches the threshold and back off 8 °C lower. Runs while this app is open; it never switches off a boost you set yourself."));

        let limits = section(p, "Manual fan speed and custom fan curves: unavailable").child(note(
            p,
            "Why: this EC's user fan tables are empty (curve RAM 3840-4095 reads all zero, so there is nothing to base a curve on or restore) and the manual levels would read those empty tables (registers 1859-1862 are GPU power settings on this model). Writing them could tell the fans to stop, so only Auto and Boost are offered.",
        ));

        div().flex().flex_col().gap_3().child(mode).child(fans).child(auto).child(limits).into_any_element()
    }

    fn keyboard_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        if !s.has_keyboard {
            return section(p, "Keyboard backlight: unavailable")
                .child(note(p, "The EC reports no RGB keyboard on this machine."))
                .into_any_element();
        }
        let hsv = s.hsv;
        let full = hsv_hex(hsv.h, hsv.s, 100);

        // Presets (hue + saturation; the brightness you chose is kept).
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

        // Colour picker: a hue strip and a saturation strip, each cell painted with its colour.
        let strip = |id: &'static str, cells: Vec<(u32, bool)>, pick: Rc<dyn Fn(usize) -> Job>| {
            div().flex().gap_px().children(cells.into_iter().enumerate().map(|(i, (hex, selected))| {
                let pick = pick.clone();
                div()
                    .id((id, i))
                    .flex_1()
                    .h(px(26.))
                    .cursor_pointer()
                    .bg(rgb(hex))
                    .border_2()
                    .border_color(rgb(if selected { p.fg } else { hex }))
                    .on_click(cx.listener(move |ui, _, _, cx| {
                        let job = pick(i);
                        ui.act(cx, job);
                    }))
            }))
        };
        let hue_sel = usize::from(hsv.h / 10) % 36;
        let hues = strip(
            "kb-hue",
            (0..36).map(|i| (hsv_hex(i * 10, 100, 100), i as usize == hue_sel)).collect(),
            Rc::new(move |i| {
                let h = i as u16 * 10;
                Box::new(move |hw| {
                    let cur = hw.hsv();
                    hw.set_keyboard(Hsv { h, ..cur })
                })
            }),
        );
        let sat_sel = (usize::from(hsv.s) + 5) / 10;
        let sats = strip(
            "kb-sat",
            (0..=10).map(|i| (hsv_hex(hsv.h, i as u8 * 10, 100), i == sat_sel)).collect(),
            Rc::new(move |i| {
                let sat = i as u8 * 10;
                Box::new(move |hw| {
                    let cur = hw.hsv();
                    hw.set_keyboard(Hsv { s: sat, ..cur })
                })
            }),
        );

        // Brightness: the EC only stores colour levels, so this scales them.
        let steps: Vec<u8> = (0..=100).step_by(10).collect();
        let labels = steps.iter().map(|v| v.to_string()).collect();
        let sel = (usize::from(hsv.v) + 5) / 10;
        let brightness = self.stepped(cx, "kb-v", labels, sel, true, move |i| {
            let v = i as u8 * 10;
            Box::new(move |hw| {
                let cur = hw.hsv();
                hw.set_keyboard(Hsv { v, ..cur })
            })
        });

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                section(p, "Keyboard backlight colour")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().size_8().rounded_md().bg(rgb(full)).border_1().border_color(rgb(p.border)))
                            .child(format!("#{full:06x} · hue {}° · saturation {} %", hsv.h, hsv.s)),
                    )
                    .child(swatches),
            )
            .child(
                section(p, "Colour picker")
                    .child(div().text_color(rgb(p.muted)).child("Hue"))
                    .child(hues)
                    .child(div().text_color(rgb(p.muted)).child("Saturation"))
                    .child(sats),
            )
            .child(
                section(p, "Brightness")
                    .child(row(p, "Backlight brightness", format!("{} %", hsv.v)))
                    .child(brightness)
                    .child(note(p, "The firmware has no separate brightness register for this keyboard, so this dims the colour itself (EC registers 1897-1899 plus the apply bit). The Fn brightness keys keep working on top of it.")),
            )
            .into_any_element()
    }

    fn system_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p();
        let s = &self.snap;
        let describe = |name: &str| match name {
            "fn_lock" => ("Fn lock", "F-keys act as media keys unless Fn is held"),
            "super_key_enable" => ("Super key", "Enable the Super (Windows) key"),
            "rainbow_animation" => ("Lightbar rainbow", "Rainbow animation on the lightbar"),
            "breathing_in_suspend" => ("Lightbar breathing", "Breathing animation while suspended"),
            _ => ("", ""),
        };
        let mut toggles = section(p, "Keyboard & platform");
        if s.toggles.is_empty() {
            toggles = toggles.child(note(p, "none exposed"));
        }
        for (i, (name, on)) in s.toggles.iter().copied().enumerate() {
            let (label, help) = describe(name);
            let label = if label.is_empty() { name } else { label };
            toggles = toggles.child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_4()
                    .child(div().flex().flex_col().child(label).child(note(p, help)))
                    .child(button(p, ("toggle", i), if on { "On" } else { "Off" }, on).on_click(
                        cx.listener(move |ui, _, _, cx| ui.act(cx, move |hw| hw.set_toggle(name, !on))),
                    )),
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
            page = page.child(section(p, "Lightbar").child(note(
                p,
                "This laptop has no lightbar (EC support flag clear, same as the stock app's LightbarSupport = 0). The kernel driver still exposes one, so its controls are hidden. The keyboard backlight colour is on the Keyboard tab.",
            )));
        }
        page.into_any_element()
    }

    /// Font select: same popover pattern as the profile dropdown; every entry is drawn in its
    /// own font. The first entry is the default, the system monospace font.
    fn font_dropdown(&self, cx: &mut Context<Self>) -> Div {
        const W: f32 = 400.;
        let p = self.p();
        let open = self.font_open;
        let default_label = format!("Monospace (system default: {})", self.system_font);
        let current = self.settings.font.clone().unwrap_or_else(|| default_label.clone());

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
                cx.notify();
            }))
            .child(div().truncate().font_family(self.family()).child(current))
            .child(div().text_color(rgb(p.muted)).child(if open { "▴" } else { "▾" }));

        let mut wrap = div().relative().w(px(W)).child(trigger);
        if open {
            let mut list = div()
                .id("font-list")
                .absolute()
                .top(px(36.))
                .left_0()
                .w(px(W))
                .max_h(px(300.))
                .overflow_y_scroll()
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
                    ui.font_open = false;
                    cx.notify();
                }));
            let entry = |i: usize, label: String, family: SharedString, pick: Option<String>, active: bool| {
                div()
                    .id(("font", i))
                    .px_2()
                    .py_1()
                    .flex()
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
            };
            list = list.child(entry(0, default_label, self.system_font.clone(), None, self.settings.font.is_none()));
            for (i, name) in self.fonts.iter().cloned().enumerate() {
                let active = self.settings.font.as_deref() == Some(name.as_str());
                list = list.child(entry(i + 1, name.clone(), SharedString::from(name.clone()), Some(name), active));
            }
            if self.fonts.is_empty() {
                list = list.child(div().p_2().child(note(p, "fontconfig (fc-list) returned no fonts")));
            }
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
            .child(div().font_family(self.family()).child("The quick brown fox · 0123456789 · PL1 35 W / PL2 35 W"))
            .child(button(p, "font-all", if self.settings.all_fonts { "Listing all fonts" } else { "Listing monospace fonts only" }, self.settings.all_fonts).on_click(
                cx.listener(|ui, _, _, cx| {
                    ui.settings.all_fonts = !ui.settings.all_fonts;
                    ui.fonts = settings::installed_fonts(ui.settings.all_fonts);
                    ui.save_settings(cx);
                }),
            ))
            .child(note(p, format!("{} families available. Default: the system monospace font. Saved in /etc/ripple-nova/gui.json.", self.fonts.len())));

        let startup = section(p, "Start at login")
            .child(
                div().flex().items_center().gap_3()
                    .child(button(p, "autostart", if self.autostart { "On" } else { "Off" }, self.autostart).on_click(cx.listener(|ui, _, _, cx| ui.toggle_autostart(cx))))
                    .child("Start Ripple Control Center when I log in"),
            )
            .child(note(p, "Starts the app minimized so the mode button is followed and the mode lights match the profile after every boot. No systemd service: it is an autostart entry (/etc/xdg/autostart/ripple-nova.desktop). The app needs root, so you will get a polkit password prompt at each login. It runs a root-owned copy installed in /usr/local/lib/ripple-nova (so an unprivileged process cannot swap it); switch the option off and on after rebuilding to refresh that copy."));

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
            .child(self.titlebar())
            .child(self.tab_bar(cx))
            .child(div().id("content").flex_1().min_h_0().overflow_y_scroll().p_4().child(body))
            .child(self.status_bar())
            .children(resize_handles())
    }
}

// ------------------------------------------------------------------ startup

/// Follow the hardware mode button (WMI event 176 = key 184): each press cycles the profile,
/// exactly what the stock Windows service does. Runs for the life of the process.
fn spawn_mode_button(hw: Arc<Mutex<Hw>>) {
    std::thread::spawn(move || {
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
    if let Err(e) = ripple_hw::require_root(true) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cmd) = args.iter().position(|a| a == "--autostart").and_then(|i| args.get(i + 1)) {
        let layout = autostart::Layout::system();
        let res = match cmd.as_str() {
            "on" => std::env::current_exe().and_then(|exe| layout.enable(&exe)),
            "off" => layout.disable(),
            "status" => Ok(()),
            other => {
                eprintln!("error: --autostart takes on|off|status, got '{other}'");
                std::process::exit(2);
            }
        };
        match res {
            Ok(()) => println!("start at login: {}", if layout.is_enabled() { "on" } else { "off" }),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let hw = Arc::new(Mutex::new(Hw::open()));
    spawn_mode_button(hw.clone());
    let start = Start::from_args();
    let minimized = start.minimized;
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
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
                cx.new(|cx| Ui::new(hw, start, cx))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
