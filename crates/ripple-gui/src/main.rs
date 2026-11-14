//! `rcc-gui` — Ripple Control Center GPUI app. Root only (EC access).
//!
//! Zed-style window: client-side decorations (own titlebar, no GNOME bar), tabs, status bar,
//! One Dark / One Light palettes, a user-selectable font (system monospace by default). All
//! hardware I/O runs on GPUI's background executor behind one mutex, so the render path
//! never blocks.

mod autostart;
mod chart;
mod chrome;
mod drag;
mod hw;
mod input;
mod remote;
mod settings;
mod slider;
mod tabs;
mod theme;
mod widgets;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    canvas, deferred, div, fill, img, point, prelude::*, px, quad, relative, rgb, rgba, size,
    AnyElement, App, Application, BorderStyle, Bounds, Context, Corners, Div, Edges, ElementId,
    FocusHandle, Image, ImageFormat, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    PathBuilder, Pixels, ResizeEdge, SharedString, Stateful, TitlebarOptions, Window, WindowBounds,
    WindowDecorations, WindowOptions,
};
use hw::{FanMode, Field, Hw, Snapshot, STOCK_PROFILES};
use input::{Outcome, TextInput};
use ripple_core::modes::OperatingMode;
use ripple_hw::control::{self, Hsv, Profile};
use ripple_hw::notify::{ensure_tray, popups_enabled, toggle_label};
use settings::Settings;
use slider::*;
use theme::*;
use widgets::*;

/// `rcc-gui [--tab profiles|fan|keyboard|system|settings] [--open] [--light] [--size WxH]`:
/// where to start (handy for screenshots; `--light` forces the light theme for this run).
/// `rcc-gui --autostart on|off|status` installs/removes/shows the open-at-login entry and exits.
#[derive(Clone, Copy)]
struct Start {
    tab: Tab,
    open: bool,
    light: bool,
    size: Option<(f32, f32)>,
}

impl Start {
    /// A window opened later (by `show`): the defaults.
    fn plain() -> Self {
        Self {
            tab: Tab::Profiles,
            open: false,
            light: false,
            size: None,
        }
    }

    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let tab = match args
            .iter()
            .position(|a| a == "--tab")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
        {
            Some("fan") => Tab::Fan,
            Some("keyboard") => Tab::Keyboard,
            Some("graphics") => Tab::Graphics,
            Some("battery") => Tab::Battery,
            Some("device") => Tab::Device,
            Some("system") => Tab::System,
            Some("settings") => Tab::Settings,
            _ => Tab::Profiles,
        };
        let size = args
            .iter()
            .position(|a| a == "--size")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            });
        Self {
            tab,
            open: args.iter().any(|a| a == "--open"),
            light: args.iter().any(|a| a == "--light"),
            size,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Profiles,
    Fan,
    Graphics,
    Keyboard,
    Battery,
    Device,
    System,
    Settings,
}

type Job = Box<dyn FnOnce(&mut Hw) -> Result<String, String> + Send>;
/// Builds the hardware job for a slider position.
type Pick = Rc<dyn Fn(usize) -> Job>;

// ------------------------------------------------------------------ slider

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
    /// The profile whose Delete button was clicked once (a second click deletes it).
    confirm_delete: Option<usize>,
    /// The two text fields: the keyboard colour as hex, and the font family.
    hex: TextInput,
    font: TextInput,
    hex_focus: FocusHandle,
    font_focus: FocusHandle,
    /// Whether each is being edited; set at the top of every render, which has the window.
    hex_focused: bool,
    font_focused: bool,
    /// The app icon of the title bar, built once: `img` caches the rendering by the image's id.
    icon: Arc<Image>,
    /// Highlighted row of the font typeahead.
    font_sel: usize,
    /// Is the open-at-login entry installed?
    autostart: bool,
    /// Profile under the mouse in the dropdown, shown in the "what changes" panel.
    preview: Option<Profile>,
    sliders: Rc<RefCell<HashMap<&'static str, SliderReg>>>,
    /// The slider being dragged and its debounce state.
    slider: drag::SliderDrag,
}

impl Ui {
    fn new(shared: Arc<Shared>, start: Start, cx: &mut Context<Self>) -> Self {
        let hw = shared.hw.clone();
        let latest = shared.clone();
        // The hardware is polled by a background thread; this only mirrors the latest snapshot. Err =
        // the view is gone, stop.
        cx.spawn(async move |this, cx| loop {
            let snap = latest.latest();
            if this
                .update(cx, |ui, cx| {
                    ui.snap = snap;
                    // the tray icon's menu and `rcc popups` change it too; a stale copy would also be saved back
                    ui.settings.popups = popups_enabled();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
        })
        .detach();
        let mut settings = Settings::load();
        settings.light |= start.light;
        Self {
            hw,
            snap: shared.latest(),
            shared,
            tab: start.tab,
            fonts: settings::installed_fonts(),
            settings,
            system_font: settings::system_mono().into(),
            dropdown_open: start.open && start.tab == Tab::Profiles,
            confirm_delete: None,
            hex: TextInput::new(|c| c == '#' || c.is_ascii_hexdigit(), 7),
            font: TextInput::new(|c| !c.is_control(), 64),
            hex_focus: cx.focus_handle(),
            font_focus: cx.focus_handle(),
            hex_focused: false,
            font_focused: false,
            icon: Arc::new(Image::from_bytes(
                ImageFormat::Svg,
                include_bytes!("../../../dist/ripple-nova.svg").to_vec(),
            )),
            font_sel: 0,
            autostart: autostart::Layout::system().is_enabled(),
            preview: None,
            sliders: Rc::new(RefCell::new(HashMap::new())),
            slider: drag::SliderDrag::default(),
        }
    }

    fn p(&self) -> Palette {
        if self.settings.light {
            Palette::light()
        } else {
            Palette::dark()
        }
    }

    fn family(&self) -> SharedString {
        self.settings
            .font
            .clone()
            .map_or_else(|| self.system_font.clone(), Into::into)
    }

    /// Run a hardware action off the UI thread, then refresh the snapshot.
    fn act(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(&mut Hw) -> Result<String, String> + Send + 'static,
    ) {
        self.confirm_delete = None;
        self.run(cx, job, false);
    }

    /// `act`, counting it as a slider job when `slider` is set: the thumb is held until the last one lands.
    fn run(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(&mut Hw) -> Result<String, String> + Send + 'static,
        slider: bool,
    ) {
        if slider {
            self.slider.job_started();
        }
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
                ui.set_snap(snap, cx);
                if slider {
                    ui.slider.job_done();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The snapshot after a click: shown at once and published (popups) like the poller's.
    fn set_snap(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        self.shared.publish(snap.clone());
        self.snap = snap;
        cx.notify();
    }

    /// Delete on the second click within a few seconds; the first only arms the button.
    fn delete_clicked(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.confirm_delete == Some(idx) {
            self.act(cx, move |hw| hw.delete_profile(idx));
            return;
        }
        self.confirm_delete = Some(idx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            this.update(cx, |ui, cx| {
                if ui.confirm_delete == Some(idx) {
                    ui.confirm_delete = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Switch the open-at-login entry; the result (or error) goes to the status bar.
    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let layout = autostart::Layout::system();
        let res = if layout.is_enabled() {
            layout.disable().map(|()| "open at login: off".to_string())
        } else {
            std::env::current_exe()
                .and_then(|exe| {
                    layout.enable(&exe, &autostart_tray(), self.settings.start_minimized)
                })
                .map(|()| "open at login: on".to_string())
        };
        self.hw.lock().unwrap_or_else(|e| e.into_inner()).event =
            res.unwrap_or_else(|e| format!("error: open at login: {e}"));
        self.autostart = layout.is_enabled();
        cx.notify();
    }

    /// "Start minimized" (tray icon only at login): saved, and the login script is rewritten when
    /// open-at-login is on.
    fn toggle_start_minimized(&mut self, cx: &mut Context<Self>) {
        self.settings.start_minimized = !self.settings.start_minimized;
        let layout = autostart::Layout::system();
        if layout.is_enabled() {
            let res = std::env::current_exe().and_then(|exe| {
                layout.enable(&exe, &autostart_tray(), self.settings.start_minimized)
            });
            if let Err(e) = res {
                self.hw.lock().unwrap_or_else(|e| e.into_inner()).event =
                    format!("error: open at login: {e}");
            }
        }
        self.save_settings(cx);
    }

    /// Persist UI preferences; failures show up in the status bar.
    fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.settings.save() {
            self.hw.lock().unwrap_or_else(|e| e.into_inner()).event =
                format!("error: could not save settings: {e}");
        }
        cx.notify();
    }

    // ---- text fields

    /// Say something in the status bar.
    fn say(&mut self, msg: impl Into<String>, cx: &mut Context<Self>) {
        self.hw.lock().unwrap_or_else(|e| e.into_inner()).event = msg.into();
        cx.notify();
    }

    /// The text a field shows while it is not being edited.
    fn canonical(&self, which: InputId) -> String {
        match which {
            InputId::Hex => {
                let h = self.snap.hsv;
                format!("#{:06x}", hsv_hex(h.h, h.s, h.v))
            }
            InputId::Font => self.settings.font.clone().unwrap_or_default(),
        }
    }

    fn input_key(
        &mut self,
        which: InputId,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &ev.keystroke;
        let ctrl = k.modifiers.control || k.modifiers.platform;
        let paste = if ctrl && k.key == "v" {
            cx.read_from_clipboard().and_then(|i| i.text())
        } else {
            None
        };
        let field = match which {
            InputId::Hex => &mut self.hex,
            InputId::Font => &mut self.font,
        };
        match field.key(&k.key, k.key_char.as_deref(), ctrl, paste.as_deref()) {
            Outcome::Nothing => {}
            Outcome::Changed => self.font_sel = 0,
            // the field shows its value again on the next render, once it has lost the focus
            Outcome::Cancel => window.blur(),
            Outcome::Up => self.font_sel = self.font_sel.saturating_sub(1),
            Outcome::Down => {
                let last = self.font_rows().0.len().saturating_sub(1);
                self.font_sel = (self.font_sel + 1).min(last);
            }
            Outcome::Submit => match which {
                InputId::Hex => self.apply_hex(window, cx),
                InputId::Font => self.apply_font(window, cx),
            },
        }
        cx.notify();
    }

    /// Enter in the colour field: the typed colour becomes the keyboard colour, as `rcc rgb color` does.
    fn apply_hex(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rgb8) = parse_hex(self.hex.text()) else {
            return self.say("error: colour must be #rrggbb (or #rgb)", cx);
        };
        let levels = control::kb_from_8bit(rgb8);
        // all-zero levels would be black, which is not a colour for the EC: that is what Off is for
        if levels == (0, 0, 0) {
            return self.say(
                "error: black is not a keyboard colour, use Effect > Off",
                cx,
            );
        }
        let hsv = Hsv::from_levels(levels);
        self.act(cx, move |hw| hw.set_keyboard(hsv));
        window.blur();
    }

    /// What the font typeahead offers (`None` is the system default) and how many match in all.
    fn font_rows(&self) -> (Vec<Option<String>>, usize) {
        let typed = self.font.text();
        if typed.trim().is_empty() {
            let rows = std::iter::once(None)
                .chain(self.fonts.iter().take(FONT_ROWS - 1).cloned().map(Some))
                .collect();
            return (rows, self.fonts.len());
        }
        let (hits, total) = input::suggest(&self.fonts, typed, FONT_ROWS);
        (
            hits.into_iter().map(|h| Some(h.to_string())).collect(),
            total,
        )
    }

    /// Enter in the font field: the highlighted row (the first by default).
    fn apply_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (rows, _) = self.font_rows();
        match rows.get(self.font_sel.min(rows.len().saturating_sub(1))) {
            Some(row) => self.pick_font(row.clone(), window, cx),
            None => {
                let typed = self.font.text().to_string();
                self.say(format!("error: no installed font matches '{typed}'"), cx)
            }
        }
    }

    fn pick_font(&mut self, font: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.font = font;
        self.save_settings(cx);
        window.blur(); // the field shows the saved family again
    }

    /// A single-line text field: accent border while edited, the text with a caret, or a placeholder.
    fn text_field(
        &self,
        which: InputId,
        id: &'static str,
        width: f32,
        placeholder: String,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = self.p();
        let (input, focus, focused) = match which {
            InputId::Hex => (&self.hex, self.hex_focus.clone(), self.hex_focused),
            InputId::Font => (&self.font, self.font_focus.clone(), self.font_focused),
        };
        let mut content = div().flex().items_center().overflow_hidden();
        if input.text().is_empty() && !focused {
            content = content.text_color(rgb(p.muted)).child(placeholder);
        } else if focused && input.all_selected() {
            content = content.child(div().bg(rgb(p.accent_bg)).child(input.text().to_string()));
        } else if focused {
            let (before, after) = input.split();
            content = content
                .child(before.to_string())
                .child(div().w(px(1.)).h(px(16.)).bg(rgb(p.accent)))
                .child(after.to_string());
        } else {
            content = content.child(input.text().to_string());
        }
        let click = focus.clone();
        div()
            .id(id)
            .track_focus(&focus)
            .on_key_down(cx.listener(move |ui, ev: &KeyDownEvent, window, cx| {
                ui.input_key(which, ev, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _, window, cx| {
                    click.focus(window);
                    cx.notify();
                }),
            )
            .w(px(width))
            .h(px(32.))
            .px_3()
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(if focused { p.accent } else { p.border }))
            .bg(rgb(p.panel))
            .cursor_text()
            .child(content)
    }

    // ---- slider pointer handling
}

// ------------------------------------------------------------------ small widgets

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputId {
    Hex,
    Font,
}

/// Rows the font typeahead shows at most.
const FONT_ROWS: usize = 8;

/// `#rrggbb` or `#rgb` (the `#` is optional) as 8-bit channels.
fn parse_hex(text: &str) -> Option<(u8, u8, u8)> {
    let t = text.trim().trim_start_matches('#');
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let two = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
    let one = |i: usize| u8::from_str_radix(&t[i..i + 1], 16).ok().map(|d| d * 17);
    match t.len() {
        6 => Some((two(0)?, two(2)?, two(4)?)),
        3 => Some((one(0)?, one(1)?, one(2)?)),
        _ => None,
    }
}

/// Display colour (0xRRGGBB) of an HSV choice, via the EC's own level maths.
fn hsv_hex(h: u16, sat: u8, v: u8) -> u32 {
    let (r, g, b) = Hsv { h, s: sat, v }.to_levels();
    let f = |l: u8| u32::from(l) * 255 / u32::from(control::KB_LEVEL_MAX);
    (f(r) << 16) | (f(g) << 8) | f(b)
}

impl Render for Ui {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // a field that is not being edited shows the saved value
        self.hex_focused = self.hex_focus.is_focused(window);
        self.font_focused = self.font_focus.is_focused(window);
        if !self.hex_focused {
            let t = self.canonical(InputId::Hex);
            self.hex.set_text(&t);
        }
        if !self.font_focused {
            let t = self.canonical(InputId::Font);
            self.font.set_text(&t);
        }
        let p = self.p();
        let body = match self.tab {
            Tab::Profiles => self.profiles_tab(cx),
            Tab::Fan => self.fan_tab(cx),
            Tab::Keyboard => self.keyboard_tab(cx),
            Tab::Graphics => self.graphics_tab(cx),
            Tab::Battery => self.battery_tab(cx),
            Tab::Device => self.device_tab(cx),
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
            .text_size(px(TEXT_BODY))
            .border_1()
            .border_color(rgb(p.border))
            // slider drags continue outside the slider, anywhere in the window
            .on_mouse_move(cx.listener(|ui, e: &MouseMoveEvent, _, cx| {
                if ui.slider.down {
                    if e.pressed_button == Some(MouseButton::Left) {
                        ui.slider_drag((f32::from(e.position.x), f32::from(e.position.y)), cx);
                    } else {
                        ui.slider_release(cx);
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ui, _, _, cx| ui.slider_release(cx)),
            )
            .child(self.titlebar(window))
            .child(self.tab_bar(cx))
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(body),
            )
            .child(self.status_bar())
            .children(resize_handles())
    }
}

// ------------------------------------------------------------------ startup

/// The tray the login entry starts: the installed one, else the one beside this binary (a build directory).
fn autostart_tray() -> std::path::PathBuf {
    ["/usr/local/bin/rcc-tray", "/usr/bin/rcc-tray"]
        .map(std::path::PathBuf::from)
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(tray_binary)
}

/// Where `rcc-tray` lives: next to this binary (installed, or a build directory), else the usual places.
fn tray_binary() -> std::path::PathBuf {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("rcc-tray")));
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
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(ripple_hw::notify::GUI_LOCK)
    else {
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
    if let Some(cmd) = args
        .iter()
        .position(|a| a == "--autostart")
        .and_then(|i| args.get(i + 1))
    {
        let layout = autostart::Layout::system();
        let res = match cmd.as_str() {
            "on" => std::env::current_exe().and_then(|exe| {
                layout.enable(&exe, &autostart_tray(), Settings::load().start_minimized)
            }),
            "off" => layout.disable(),
            "status" => Ok(()),
            other => {
                eprintln!("error: --autostart takes on|off|status, got '{other}'");
                std::process::exit(2);
            }
        };
        match res {
            Ok(()) => println!(
                "open at login: {}",
                if layout.is_enabled() { "on" } else { "off" }
            ),
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
    let commands = remote::listen()
        .map_err(|e| eprintln!("warning: control socket: {e}"))
        .ok();
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
        if !open_main_window(cx, shared.clone(), start) {
            cx.quit();
            return;
        }
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
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
            })
            .detach();
        }
    });
}

/// Open the window; false (after saying why) when the display refuses it.
fn open_main_window(cx: &mut App, shared: Arc<Shared>, start: Start) -> bool {
    let (w, h) = start.size.unwrap_or((860., 640.));
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let opened = cx.open_window(
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
            let ui = cx.new(|cx| Ui::new(shared, start, cx));
            if start.open {
                // --open: the field of the tab, so it can be tried (and screenshotted) in its editing state
                ui.update(cx, |ui, _| match start.tab {
                    Tab::Settings => window.focus(&ui.font_focus),
                    Tab::Keyboard => window.focus(&ui.hex_focus),
                    _ => {}
                });
            }
            ui
        },
    );
    if let Err(e) = &opened {
        eprintln!("error: could not open the window: {e}");
    }
    opened.is_ok()
}

/// `show` (the tray click, the menu launcher): get a usable window in front of the user. Measured on native
/// Wayland: `activate_window` on a *minimized* window does nothing (GNOME only posts a "... is ready"
/// notice) and a client cannot un-minimize itself. So a window that is not the active one is replaced by a
/// fresh one. The new window opens first: gpui stops the whole event loop when the window count hits zero.
fn show_window(cx: &mut App, shared: Arc<Shared>) {
    let old = cx.windows();
    let active = old.iter().any(|w| {
        w.update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    });
    if active {
        return;
    }
    if !open_main_window(cx, shared, Start::plain()) {
        return; // keep the old one: it is better than none
    }
    for w in old {
        let _ = w.update(cx, |_, window, _| window.remove_window());
    }
    cx.activate(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_parse_with_or_without_the_hash_and_in_short_form() {
        assert_eq!(parse_hex("#ff8800"), Some((255, 136, 0)));
        assert_eq!(parse_hex("ff8800"), Some((255, 136, 0)));
        assert_eq!(parse_hex("  #F80 "), Some((255, 136, 0)));
        assert_eq!(parse_hex("#12"), None);
        assert_eq!(parse_hex("#ff88000"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("#"), None);
    }
}
