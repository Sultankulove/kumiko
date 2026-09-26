//! Colours of the workbench, resolved once from the active theme (spec, section 21). A token
//! the theme does not define falls back to a related token, then to a built-in default that
//! suits the theme's base, so a partial theme still gives a usable window.

use kumiko_core::resources::{Color, Theme, ThemeBase};

/// `0xRRGGBBAA`, as GPUI's `rgba` takes it.
pub type Rgba = u32;

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub foreground: Rgba,
    pub muted: Rgba,
    pub window: Rgba,
    pub menu_bar: Rgba,
    pub toolbar: Rgba,
    pub stripe: Rgba,
    pub stripe_active: Rgba,
    pub panel: Rgba,
    pub panel_header: Rgba,
    pub editor: Rgba,
    pub editor_foreground: Rgba,
    pub tab_active: Rgba,
    pub tab_inactive: Rgba,
    pub tab_underline: Rgba,
    pub selection: Rgba,
    pub selection_inactive: Rgba,
    pub hover: Rgba,
    pub status_bar: Rgba,
    pub status_foreground: Rgba,
    pub popup: Rgba,
    pub input: Rgba,
    pub border: Rgba,
    pub focus_border: Rgba,
    pub accent: Rgba,
    pub error: Rgba,
    pub warning: Rgba,
    pub info: Rgba,
}

fn pack(color: Color) -> Rgba {
    u32::from_be_bytes([color.r, color.g, color.b, color.a])
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Self {
        Self::resolve(|token| theme.color(token), theme.base != ThemeBase::Light)
    }

    /// The built-in dark colours, for when no theme can be loaded.
    pub fn fallback() -> Self {
        Self::resolve(|_| None, true)
    }

    fn resolve(color: impl Fn(&str) -> Option<Color>, dark: bool) -> Self {
        // The first token the theme defines, else the default for its base.
        let pick = |tokens: &[&str], dark_default: Rgba, light_default: Rgba| {
            tokens
                .iter()
                .find_map(|t| color(t))
                .map_or(if dark { dark_default } else { light_default }, pack)
        };
        let window = pick(
            &["window.background", "panel.background"],
            0x3C3F41FF,
            0xF2F2F2FF,
        );
        let foreground = pick(&["foreground", "editor.foreground"], 0xBBBBBBFF, 0x000000FF);
        Self {
            foreground,
            muted: pick(&["foreground.muted"], 0x8C8C8CFF, 0x6E6E6EFF),
            window,
            menu_bar: pick(
                &[
                    "menuBar.background",
                    "window.background",
                    "panel.background",
                ],
                window,
                window,
            ),
            toolbar: pick(
                &[
                    "toolbar.background",
                    "window.background",
                    "panel.background",
                ],
                window,
                window,
            ),
            stripe: pick(
                &[
                    "toolStripe.background",
                    "window.background",
                    "sideBar.background",
                ],
                window,
                window,
            ),
            stripe_active: pick(
                &["toolStripe.button.active", "list.hover"],
                0x2D2F30FF,
                0xD5D5D5FF,
            ),
            panel: pick(&["panel.background", "sideBar.background"], window, window),
            panel_header: pick(&["panel.header", "panel.background"], window, window),
            editor: pick(&["editor.background"], 0x2B2B2BFF, 0xFFFFFFFF),
            editor_foreground: pick(&["editor.foreground", "foreground"], foreground, foreground),
            tab_active: pick(
                &["tab.active.background", "editor.background"],
                0x4E5254FF,
                0xFFFFFFFF,
            ),
            tab_inactive: pick(
                &["tab.inactive.background", "panel.background"],
                window,
                window,
            ),
            tab_underline: pick(
                &["tab.active.underline", "accent", "focusBorder"],
                0x4A88C7FF,
                0x4083C9FF,
            ),
            selection: pick(&["list.selection", "accent"], 0x2F65CAFF, 0x3875D6FF),
            selection_inactive: pick(
                &["list.selection.inactive", "editor.lineHighlight"],
                0x0D293EFF,
                0xD5D5D5FF,
            ),
            hover: pick(
                &["list.hover", "editor.lineHighlight"],
                0x464A4DFF,
                0xE8E8E8FF,
            ),
            status_bar: pick(
                &["statusBar.background", "window.background"],
                window,
                window,
            ),
            status_foreground: pick(
                &["statusBar.foreground", "foreground"],
                foreground,
                foreground,
            ),
            popup: pick(&["popup.background", "panel.background"], window, window),
            input: pick(
                &["input.background", "editor.background"],
                0x45494AFF,
                0xFFFFFFFF,
            ),
            border: pick(&["border"], 0x323232FF, 0xC9C9C9FF),
            focus_border: pick(&["focusBorder", "accent"], 0x3D6185FF, 0x87AFDAFF),
            accent: pick(&["accent", "focusBorder"], 0x4A88C7FF, 0x4083C9FF),
            error: pick(&["error"], 0xBC3F3CFF, 0xC75450FF),
            warning: pick(&["warning"], 0xBE9117FF, 0xB58900FF),
            info: pick(&["info"], 0x3592C4FF, 0x3574F0FF),
        }
    }
}

#[cfg(test)]
mod tests {
    use kumiko_core::resources::ResourceRegistry;

    use super::*;

    fn registry() -> ResourceRegistry {
        let registry = ResourceRegistry::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources");
        assert!(registry.scan(&[root]).is_empty());
        registry
    }

    #[test]
    fn darcula_colours_come_from_the_theme() {
        let theme = registry().theme("kumiko.theme.darcula").unwrap();
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.editor, 0x2B2B2BFF);
        assert_eq!(palette.window, 0x3C3F41FF);
        assert_eq!(palette.selection, 0x2F65CAFF);
        assert_eq!(palette.tab_underline, 0x4A88C7FF);
    }

    #[test]
    fn missing_tokens_fall_back_to_related_ones() {
        // Hinoki names only the common tokens; the Classic UI ones fall back.
        let theme = registry().theme("kumiko.theme.hinoki-dark").unwrap();
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.window, 0x18181DFF);
        assert_eq!(palette.menu_bar, palette.window);
        assert_eq!(palette.tab_underline, 0x2A9D8FFF);
        assert_eq!(palette.status_bar, 0x2A9D8FFF);
    }
}
