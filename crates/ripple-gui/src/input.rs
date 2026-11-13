//! Editing state of a single-line text field, without any GPUI types so it can be tested. GPUI has no
//! ready-made input widget; the window feeds this the keystrokes and draws the result.
//!
//! No selection beyond "select all" (Ctrl+A), no IME: the fields are a hex colour and a font name.

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// The text changed.
    Changed,
    Submit,
    Cancel,
    Up,
    Down,
}

pub struct TextInput {
    text: String,
    /// Byte offset of the caret, always on a char boundary.
    cursor: usize,
    /// Ctrl+A: the whole text is selected, the next typing replaces it.
    all: bool,
    /// Which characters are accepted (typed or pasted).
    accept: fn(char) -> bool,
    /// Most characters the field holds.
    max: usize,
}

impl TextInput {
    pub fn new(accept: fn(char) -> bool, max: usize) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            all: false,
            accept,
            max,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text before and after the caret.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.cursor)
    }

    pub fn all_selected(&self) -> bool {
        self.all
    }

    /// Replace the text from outside (the value the field shows while it is not being edited). Does
    /// nothing when it is the same text, so the caret stays where it is.
    pub fn set_text(&mut self, text: &str) {
        if self.text != text {
            self.text = text.to_string();
            self.cursor = self.text.len();
            self.all = false;
        }
    }

    /// One keystroke: `key` as GPUI names it ("enter", "backspace", "left", "a", ...), `ch` the printable
    /// character it types, `ctrl` Ctrl (or the platform key), `paste` the clipboard text (read only when
    /// the keystroke is Ctrl+V).
    pub fn key(&mut self, key: &str, ch: Option<&str>, ctrl: bool, paste: Option<&str>) -> Outcome {
        if ctrl {
            return match key {
                "a" if !self.text.is_empty() => {
                    self.all = true;
                    Outcome::Nothing
                }
                "v" => paste.map_or(Outcome::Nothing, |t| self.insert(t)),
                _ => Outcome::Nothing,
            };
        }
        match key {
            "enter" => Outcome::Submit,
            "escape" => Outcome::Cancel,
            "up" => Outcome::Up,
            "down" => Outcome::Down,
            "backspace" => self.erase(false),
            "delete" => self.erase(true),
            "left" => self.step(false),
            "right" => self.step(true),
            "home" => self.jump(0),
            "end" => self.jump(self.text.len()),
            _ => ch.map_or(Outcome::Nothing, |c| self.insert(c)),
        }
    }

    fn insert(&mut self, typed: &str) -> Outcome {
        let room = self.max.saturating_sub(if self.all {
            0
        } else {
            self.text.chars().count()
        });
        let add: String = typed
            .chars()
            .filter(|c| (self.accept)(*c))
            .take(room)
            .collect();
        if add.is_empty() {
            return Outcome::Nothing;
        }
        if self.all {
            self.text.clear();
            self.cursor = 0;
            self.all = false;
        }
        self.text.insert_str(self.cursor, &add);
        self.cursor += add.len();
        Outcome::Changed
    }

    fn erase(&mut self, forward: bool) -> Outcome {
        if self.all {
            self.all = false;
            self.text.clear();
            self.cursor = 0;
            return Outcome::Changed;
        }
        if forward {
            if let Some(c) = self.text[self.cursor..].chars().next() {
                self.text.drain(self.cursor..self.cursor + c.len_utf8());
                return Outcome::Changed;
            }
        } else if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.drain(self.cursor..self.cursor + c.len_utf8());
            return Outcome::Changed;
        }
        Outcome::Nothing
    }

    fn step(&mut self, forward: bool) -> Outcome {
        self.all = false;
        if forward {
            if let Some(c) = self.text[self.cursor..].chars().next() {
                self.cursor += c.len_utf8();
            }
        } else if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
        Outcome::Nothing
    }

    fn jump(&mut self, to: usize) -> Outcome {
        self.all = false;
        self.cursor = to;
        Outcome::Nothing
    }
}

/// The installed families that match what was typed, best first (an exact match, then those that start
/// with it, then those that contain it; case ignored), at most `limit`, plus how many match in all.
/// Nothing typed matches nothing: the caller shows its own default row.
pub fn suggest<'a>(fonts: &'a [String], typed: &str, limit: usize) -> (Vec<&'a str>, usize) {
    let q = typed.trim().to_lowercase();
    if q.is_empty() {
        return (Vec::new(), 0);
    }
    let rank = |name: &str| {
        let n = name.to_lowercase();
        if n == q {
            Some(0)
        } else if n.starts_with(&q) {
            Some(1)
        } else if n.contains(&q) {
            Some(2)
        } else {
            None
        }
    };
    let mut hits: Vec<(u8, &str)> = fonts
        .iter()
        .filter_map(|f| rank(f).map(|r| (r, f.as_str())))
        .collect();
    hits.sort_by_key(|(r, _)| *r); // stable: the list's own order within a rank
    let total = hits.len();
    (
        hits.into_iter().take(limit).map(|(_, f)| f).collect(),
        total,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any(_: char) -> bool {
        true
    }

    fn hex(c: char) -> bool {
        c == '#' || c.is_ascii_hexdigit()
    }

    fn typed(t: &mut TextInput, s: &str) {
        for c in s.chars() {
            let k = c.to_string();
            let key = if c == ' ' { "space" } else { k.as_str() };
            t.key(key, Some(&k), false, None);
        }
    }

    #[test]
    fn typing_and_editing_around_the_caret() {
        let mut t = TextInput::new(any, 40);
        typed(&mut t, "Fira Code");
        assert_eq!(t.text(), "Fira Code");
        assert_eq!(t.key("left", None, false, None), Outcome::Nothing);
        t.key("left", None, false, None);
        t.key("left", None, false, None);
        t.key("left", None, false, None); // four steps back: before "Code"
        typed(&mut t, "Mono ");
        assert_eq!(t.text(), "Fira Mono Code");
        assert_eq!(t.split(), ("Fira Mono ", "Code"));
        assert_eq!(t.key("backspace", None, false, None), Outcome::Changed);
        assert_eq!(t.text(), "Fira MonoCode");
        assert_eq!(t.key("delete", None, false, None), Outcome::Changed);
        assert_eq!(t.text(), "Fira Monoode");
        t.key("home", None, false, None);
        assert_eq!(
            t.key("backspace", None, false, None),
            Outcome::Nothing,
            "nothing before the start"
        );
        t.key("end", None, false, None);
        assert_eq!(
            t.key("delete", None, false, None),
            Outcome::Nothing,
            "nothing after the end"
        );
        assert_eq!(t.split(), ("Fira Monoode", ""));
    }

    #[test]
    fn multibyte_characters_never_split() {
        let mut t = TextInput::new(any, 40);
        typed(&mut t, "aéb");
        t.key("left", None, false, None);
        assert_eq!(t.key("backspace", None, false, None), Outcome::Changed);
        assert_eq!(t.text(), "ab");
        t.key("home", None, false, None);
        typed(&mut t, "ü");
        t.key("right", None, false, None);
        assert_eq!(t.split(), ("üa", "b"));
        t.key("delete", None, false, None);
        assert_eq!(t.text(), "üa");
    }

    #[test]
    fn control_keys_report_what_the_field_should_do() {
        let mut t = TextInput::new(any, 40);
        assert_eq!(t.key("enter", None, false, None), Outcome::Submit);
        assert_eq!(t.key("escape", None, false, None), Outcome::Cancel);
        assert_eq!(t.key("up", None, false, None), Outcome::Up);
        assert_eq!(t.key("down", None, false, None), Outcome::Down);
        // keys with no character (tab, F-keys) type nothing
        assert_eq!(t.key("tab", None, false, None), Outcome::Nothing);
        assert_eq!(t.text(), "");
    }

    #[test]
    fn select_all_is_replaced_by_the_next_typing_or_cleared() {
        let mut t = TextInput::new(any, 40);
        typed(&mut t, "abc");
        t.key("a", Some("a"), true, None); // Ctrl+A, not an "a"
        assert!(t.all_selected());
        assert_eq!(t.text(), "abc");
        typed(&mut t, "x");
        assert_eq!((t.text(), t.all_selected()), ("x", false));

        typed(&mut t, "yz");
        t.key("a", None, true, None);
        assert_eq!(t.key("backspace", None, false, None), Outcome::Changed);
        assert_eq!(t.text(), "");
        // arrows end the selection without changing anything
        typed(&mut t, "ab");
        t.key("a", None, true, None);
        t.key("left", None, false, None);
        assert!(!t.all_selected());
        assert_eq!(t.text(), "ab");
        // Ctrl+A in an empty field selects nothing
        let mut e = TextInput::new(any, 4);
        e.key("a", None, true, None);
        assert!(!e.all_selected());
    }

    #[test]
    fn the_filter_and_the_length_cap_apply_to_typing_and_pasting() {
        let mut t = TextInput::new(hex, 7);
        typed(&mut t, "#ff8g0z0");
        assert_eq!(t.text(), "#ff800", "g and z are not hex digits");
        // the paste loses its non-hex characters and is cut to the one slot that is left (cap 7)
        assert_eq!(t.key("v", None, true, Some("0000zz")), Outcome::Changed);
        assert_eq!(t.text(), "#ff8000");
        assert_eq!(
            t.key("v", None, true, Some("1")),
            Outcome::Nothing,
            "full: nothing more fits"
        );
    }

    #[test]
    fn paste_replaces_a_full_selection_and_ignores_an_empty_clipboard() {
        let mut t = TextInput::new(any, 40);
        typed(&mut t, "old");
        t.key("a", None, true, None);
        assert_eq!(t.key("v", None, true, Some("new name")), Outcome::Changed);
        assert_eq!(t.text(), "new name");
        assert_eq!(t.key("v", None, true, None), Outcome::Nothing);
        assert_eq!(t.key("v", None, true, Some("")), Outcome::Nothing);
        // other Ctrl chords type nothing
        assert_eq!(t.key("x", Some("x"), true, None), Outcome::Nothing);
        assert_eq!(t.text(), "new name");
    }

    #[test]
    fn set_text_keeps_the_caret_when_nothing_changed() {
        let mut t = TextInput::new(any, 40);
        t.set_text("abc");
        t.key("left", None, false, None);
        t.set_text("abc");
        assert_eq!(t.split(), ("ab", "c"));
        t.set_text("abcd");
        assert_eq!(t.split(), ("abcd", ""));
    }

    #[test]
    fn suggestions_rank_exact_then_prefix_then_contains() {
        let fonts: Vec<String> = [
            "DejaVu Sans",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
            "Mono",
            "Monospace",
            "JetBrainsMono NF",
        ]
        .map(String::from)
        .to_vec();
        let (hits, total) = suggest(&fonts, "mono", 10);
        assert_eq!(
            hits,
            [
                "Mono",
                "Monospace",
                "DejaVu Sans Mono",
                "Noto Sans Mono",
                "JetBrainsMono NF"
            ]
        );
        assert_eq!(total, 5);
        // the cap limits the list but not the count
        let (hits, total) = suggest(&fonts, "  SANS ", 2);
        assert_eq!((hits.len(), total), (2, 3));
        assert_eq!(
            hits,
            ["DejaVu Sans", "DejaVu Sans Mono"],
            "the list's own order within a rank"
        );
        // nothing typed, nothing suggested
        assert_eq!(suggest(&fonts, "", 5), (Vec::new(), 0));
        assert_eq!(suggest(&fonts, "zzz", 5), (Vec::new(), 0));
    }
}
