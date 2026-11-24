use crate::composition::rgb;
use windows::UI::Color;

/// Light palette from the Python popup.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub white: Color,
    pub ink: Color,
    pub muted: Color,
    pub card: Color,
    pub card_hover: Color,
    pub control: Color,
    pub control_hover: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub tint: Color,
    pub tint_hover: Color,
    pub link_tint: Color,
    pub link_ink: Color,
    pub danger_tint: Color,
    pub danger: Color,
    pub soft_ink: Color,
    pub switch_off: Color,
    pub file_tint: Color,
    pub scrollbar: Color,
    pub connected: StatusColors,
    pub connecting: StatusColors,
    pub disconnected: StatusColors,
}

pub const LIGHT_THEME: Theme = Theme {
    white: rgb(255, 255, 255),
    ink: rgb(23, 32, 43),
    muted: rgb(93, 104, 117),
    card: rgb(244, 246, 249),
    card_hover: rgb(235, 239, 243),
    control: rgb(241, 243, 246),
    control_hover: rgb(228, 233, 238),
    accent: rgb(15, 110, 90),
    accent_hover: rgb(11, 88, 72),
    tint: rgb(227, 241, 236),
    tint_hover: rgb(205, 230, 221),
    link_tint: rgb(230, 235, 243),
    link_ink: rgb(61, 90, 138),
    danger_tint: rgb(253, 236, 232),
    danger: rgb(176, 42, 27),
    soft_ink: rgb(59, 70, 82),
    switch_off: rgb(211, 217, 224),
    file_tint: rgb(255, 247, 232),
    scrollbar: with_alpha(rgb(23, 32, 43), 70),
    connected: StatusColors {
        background: rgb(227, 241, 236),
        text: rgb(12, 90, 73),
        dot: rgb(31, 157, 118),
    },
    connecting: StatusColors {
        background: rgb(238, 241, 244),
        text: rgb(93, 104, 117),
        dot: rgb(138, 151, 164),
    },
    disconnected: StatusColors {
        background: rgb(251, 240, 220),
        text: rgb(138, 83, 0),
        dot: rgb(208, 138, 26),
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Connected,
    Connecting,
    Disconnected,
}

impl Status {
    pub const fn colors(self, theme: &Theme) -> StatusColors {
        match self {
            Self::Connected => theme.connected,
            Self::Connecting => theme.connecting,
            Self::Disconnected => theme.disconnected,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StatusColors {
    pub background: Color,
    pub text: Color,
    pub dot: Color,
}

pub(super) const fn with_alpha(color: Color, alpha: u8) -> Color {
    Color { A: alpha, ..color }
}
