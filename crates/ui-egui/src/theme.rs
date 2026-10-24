//! Colours and sizes. Our own palette, chosen to read like a classic desktop mail client.

use egui::Color32;

pub struct Tokens {
    pub accent: Color32,
    pub accent_dark: Color32,
    pub accent_soft: Color32,
    pub window: Color32,
    pub ribbon: Color32,
    pub tabs: Color32,
    pub pane: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub selected_unfocused: Color32,
    pub flag: Color32,
    pub danger: Color32,
    pub status: Color32,
}

pub const LIGHT: Tokens = Tokens {
    accent: Color32::from_rgb(0x1a, 0x5f, 0xb4),
    accent_dark: Color32::from_rgb(0x12, 0x47, 0x8a),
    accent_soft: Color32::from_rgb(0xd6, 0xe6, 0xf7),
    window: Color32::WHITE,
    ribbon: Color32::from_rgb(0xf5, 0xf6, 0xf7),
    tabs: Color32::from_rgb(0xf5, 0xf6, 0xf7),
    pane: Color32::from_rgb(0xfa, 0xfa, 0xfa),
    border: Color32::from_rgb(0xd8, 0xda, 0xdd),
    text: Color32::from_rgb(0x24, 0x24, 0x24),
    text_dim: Color32::from_rgb(0x6b, 0x6e, 0x73),
    hover: Color32::from_rgb(0xe8, 0xee, 0xf6),
    selected: Color32::from_rgb(0xcc, 0xe0, 0xf5),
    selected_unfocused: Color32::from_rgb(0xe3, 0xe5, 0xe8),
    flag: Color32::from_rgb(0xc4, 0x31, 0x2b),
    danger: Color32::from_rgb(0xb3, 0x26, 0x1e),
    status: Color32::from_rgb(0x1a, 0x5f, 0xb4),
};
