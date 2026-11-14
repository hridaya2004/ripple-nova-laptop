//! Colours and the type scale.

/// The type scale: body text, secondary notes, captions (axis ticks, column headers). Nothing is bold;
/// the hierarchy is size, colour and case.
pub(crate) const TEXT_BODY: f32 = 13.;

pub(crate) const TEXT_NOTE: f32 = 12.;

pub(crate) const TEXT_CAPTION: f32 = 11.;

/// Zed's One Dark / One Light, reduced to what this UI uses.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) title: u32,
    pub(crate) bg: u32,
    pub(crate) panel: u32,
    pub(crate) popup: u32,
    pub(crate) border: u32,
    pub(crate) hover: u32,
    pub(crate) fg: u32,
    pub(crate) muted: u32,
    pub(crate) accent: u32,
    pub(crate) accent_bg: u32,
    pub(crate) green: u32,
    pub(crate) red: u32,
    pub(crate) yellow: u32,
    pub(crate) err_bg: u32,
}

impl Palette {
    pub(crate) fn dark() -> Self {
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

    pub(crate) fn light() -> Self {
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
