//! Fogbreak Tidal Strata palette overlay for egui.

use egui::{Color32, Context, CornerRadius, Stroke, Visuals};

pub const CARMEL_STONE: Color32 = Color32::from_rgb(0xF7, 0xFA, 0xFC);
pub const MONTEREY_SHALE: Color32 = Color32::from_rgb(0x11, 0x18, 0x21);
pub const FOGBREAK_TEAL: Color32 = Color32::from_rgb(0x08, 0x7F, 0x9C);
pub const PANEL_LIGHT: Color32 = Color32::WHITE;
pub const PANEL_DARK: Color32 = Color32::from_rgb(0x18, 0x22, 0x2E);
pub const BORDER_LIGHT: Color32 = Color32::from_rgb(0xCB, 0xD5, 0xE1);
pub const BORDER_DARK: Color32 = Color32::from_rgb(0x2B, 0x39, 0x4B);

/// Resolve initial dark mode from `?theme=dark|light` or browser media query.
pub fn initial_dark_mode() -> bool {
    #[cfg(target_arch = "wasm32")]
    if let Some(win) = web_sys::window() {
        if let Ok(search) = win.location().search()
            && let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search)
            && let Some(t) = params.get("theme")
        {
            if t.eq_ignore_ascii_case("light") {
                return false;
            }
            if t.eq_ignore_ascii_case("dark") {
                return true;
            }
        }
        if let Ok(Some(mql)) = win.match_media("(prefers-color-scheme: dark)") {
            return mql.matches();
        }
    }
    true
}

/// Sync the HTML document's `data-theme` attribute so CSS matches egui.
pub fn sync_document_theme(dark: bool) {
    #[cfg(target_arch = "wasm32")]
    if let Some(doc) = web_sys::window().and_then(|w| w.document())
        && let Some(el) = doc.document_element()
    {
        let _ = el.set_attribute("data-theme", if dark { "dark" } else { "light" });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = dark;
}

/// Apply the Fogbreak Tidal Strata palette cleanly to `ctx`.
pub fn apply_palette(ctx: &Context, dark: bool) {
    let (bg, panel, border, text, dim) = if dark {
        (MONTEREY_SHALE, PANEL_DARK, BORDER_DARK, CARMEL_STONE, Color32::from_rgb(0x94, 0xA3, 0xB8))
    } else {
        (CARMEL_STONE, PANEL_LIGHT, BORDER_LIGHT, MONTEREY_SHALE, Color32::from_rgb(0x52, 0x62, 0x75))
    };
    let mut visuals = if dark { Visuals::dark() } else { Visuals::light() };
    visuals.panel_fill = panel;
    visuals.window_fill = bg;
    visuals.extreme_bg_color = bg;
    visuals.faint_bg_color = if dark { Color32::from_rgb(0x16, 0x20, 0x2D) } else { Color32::from_rgb(0xED, 0xF2, 0xF7) };
    visuals.window_stroke = Stroke::new(1.0, border);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(6);
    visuals.selection.bg_fill = FOGBREAK_TEAL;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = FOGBREAK_TEAL;
    visuals.widgets.noninteractive.bg_fill = panel;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.inactive.bg_fill = bg;
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, text);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.hovered.bg_fill = FOGBREAK_TEAL.gamma_multiply(0.18);
    visuals.widgets.hovered.weak_bg_fill = FOGBREAK_TEAL.gamma_multiply(0.14);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, text);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, FOGBREAK_TEAL);
    visuals.widgets.active.bg_fill = FOGBREAK_TEAL;
    visuals.widgets.active.weak_bg_fill = FOGBREAK_TEAL;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.open.bg_fill = panel;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, dim);
    ctx.set_visuals(visuals);
}
