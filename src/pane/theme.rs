//! Herdr's resolved presentation palette for plugin panes.
//!
//! Herdr snapshots its effective theme when it launches a plugin pane and provides it through
//! `HERDR_PLUGIN_PANE_THEME_JSON`. This module is the narrow wire adapter: it validates the complete
//! v1 contract, converts its lossless color representation to ratatui, and exposes only the roles
//! this popup renders. Theme resolution remains entirely Herdr-owned.

use crate::PluginError;
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

const THEME_ENV_VAR: &str = "HERDR_PLUGIN_PANE_THEME_JSON";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneTheme {
    pub(super) accent: Color,
    pub(super) panel_bg: Color,
    pub(super) surface0: Color,
    pub(super) surface_dim: Color,
    pub(super) overlay0: Color,
    pub(super) overlay1: Color,
    pub(super) text: Color,
    pub(super) subtext0: Color,
    pub(super) mauve: Color,
    pub(super) green: Color,
    pub(super) yellow: Color,
    pub(super) red: Color,
    pub(super) teal: Color,
    selection_fg: Color,
    legacy: bool,
}

impl PaneTheme {
    pub(super) fn from_env() -> Result<Self, PluginError> {
        match std::env::var(THEME_ENV_VAR) {
            Ok(json) => Self::from_json(&json),
            // Herdr releases before the pane-theme contract do not provide the snapshot. Preserve
            // the popup's established terminal-native styling until Herdr can supply exact colors.
            Err(std::env::VarError::NotPresent) => Ok(Self::legacy()),
            Err(error) => Err(PluginError::new(format!(
                "invalid {THEME_ENV_VAR}: {error}"
            ))),
        }
    }

    fn legacy() -> Self {
        Self {
            accent: Color::DarkGray,
            panel_bg: Color::Reset,
            surface0: Color::Reset,
            surface_dim: Color::DarkGray,
            overlay0: Color::DarkGray,
            overlay1: Color::Reset,
            text: Color::Reset,
            subtext0: Color::DarkGray,
            mauve: Color::Reset,
            green: Color::DarkGray,
            yellow: Color::Yellow,
            red: Color::DarkGray,
            teal: Color::Reset,
            selection_fg: Color::White,
            legacy: true,
        }
    }

    fn from_json(json: &str) -> Result<Self, PluginError> {
        let snapshot: ThemeSnapshot = serde_json::from_str(json)
            .map_err(|error| PluginError::new(format!("invalid {THEME_ENV_VAR}: {error}")))?;
        if snapshot.schema_version != 1 {
            return Err(PluginError::new(format!(
                "unsupported {THEME_ENV_VAR} schema version {}; expected 1",
                snapshot.schema_version
            )));
        }

        let PaletteSnapshot {
            accent,
            panel_bg,
            surface0,
            surface1,
            surface_dim,
            overlay0,
            overlay1,
            text,
            subtext0,
            mauve,
            green,
            yellow,
            red,
            blue,
            teal,
            peach,
        } = snapshot.palette;

        // Convert every required token, including those this first rendering pass does not style
        // independently. That keeps malformed snapshots all-or-nothing instead of accepting a
        // partial palette that only happens to satisfy today's visible roles.
        let accent = accent.into_color()?;
        let panel_bg = panel_bg.into_color()?;
        let surface0 = surface0.into_color()?;
        let _surface1 = surface1.into_color()?;
        let surface_dim = surface_dim.into_color()?;
        let overlay0 = overlay0.into_color()?;
        let overlay1 = overlay1.into_color()?;
        let text = text.into_color()?;
        let subtext0 = subtext0.into_color()?;
        let mauve = mauve.into_color()?;
        let green = green.into_color()?;
        let yellow = yellow.into_color()?;
        let red = red.into_color()?;
        let _blue = blue.into_color()?;
        let teal = teal.into_color()?;
        let _peach = peach.into_color()?;
        let selection_fg = if panel_bg == Color::Reset {
            surface_dim
        } else {
            panel_bg
        };

        Ok(Self {
            accent,
            panel_bg,
            surface0,
            surface_dim,
            overlay0,
            overlay1,
            text,
            subtext0,
            mauve,
            green,
            yellow,
            red,
            teal,
            selection_fg,
            legacy: false,
        })
    }

    /// A resolved Herdr theme is an explicit request for this full-screen TUI to render that
    /// palette. Agent launch environments commonly carry `NO_COLOR=1`; Crossterm otherwise obeys
    /// that inherited flag and silently drops every color command while still emitting modifiers.
    /// Override it only when Herdr supplied the pane-theme contract, leaving legacy launches to
    /// retain their established environment-driven behavior.
    pub(super) fn configure_terminal_output(&self) {
        if !self.legacy {
            ratatui::crossterm::style::force_color_output(true);
        }
    }

    pub(super) fn base(&self) -> Style {
        Style::new().fg(self.text).bg(self.panel_bg)
    }

    pub(super) fn heading(&self) -> Style {
        self.base().add_modifier(Modifier::BOLD)
    }

    pub(super) fn secondary(&self) -> Style {
        Style::new().fg(self.subtext0).bg(self.panel_bg)
    }

    pub(super) fn subtle(&self) -> Style {
        Style::new().fg(self.overlay0).bg(self.panel_bg)
    }

    /// Agent identity mirrors Herdr's quiet secondary identity color. Pre-contract Herdr keeps the
    /// established terminal-native foreground rather than inventing an ANSI color locally.
    pub(super) fn agent_identity(&self) -> Style {
        if self.legacy {
            self.base()
        } else {
            self.base().fg(self.teal)
        }
    }

    /// Distinct tab context gets Herdr's special-label color; pane location is quieter navigation
    /// metadata. Both remain terminal-native when no snapshot exists.
    pub(super) fn tab_label(&self) -> Style {
        if self.legacy {
            self.base()
        } else {
            self.base().fg(self.mauve)
        }
    }

    pub(super) fn pane_label(&self) -> Style {
        if self.legacy {
            self.base()
        } else {
            self.base().fg(self.overlay1)
        }
    }

    pub(super) fn separator(&self) -> Style {
        self.subtle().add_modifier(Modifier::DIM)
    }

    pub(super) fn status(&self, color: Color) -> Style {
        if self.legacy {
            self.secondary()
        } else {
            self.base().fg(color)
        }
    }

    pub(super) fn terminal_tail(&self) -> Style {
        self.subtle().add_modifier(Modifier::DIM)
    }

    pub(super) fn tab_selection(&self) -> Style {
        Style::new()
            .fg(self.panel_contrast_fg())
            .bg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// The restrained grey selection used for plain-text Queue rows. Legacy launches retain the
    /// popup's prior explicit-white foreground; resolved themes use their normal text color.
    pub(super) fn row_selection(&self) -> Style {
        let foreground = if self.legacy {
            self.selection_fg
        } else {
            self.text
        };
        Style::new()
            .fg(foreground)
            .bg(self.surface_dim)
            .add_modifier(Modifier::BOLD)
    }

    /// Grey selection treatment for content that already carries meaningful foreground hierarchy.
    /// Keeping this foreground-free lets Ratatui preserve each span's semantic color while the
    /// shared dim background still makes the two-line target read as one selected band.
    pub(super) fn selection_band(&self) -> Style {
        Style::new()
            .bg(self.surface_dim)
            .add_modifier(Modifier::BOLD)
    }

    pub(super) fn input(&self) -> Style {
        Style::new().fg(self.text).bg(self.surface0)
    }

    pub(super) fn input_placeholder(&self) -> Style {
        Style::new().fg(self.overlay0).bg(self.surface0)
    }

    pub(super) fn panel_contrast_fg(&self) -> Color {
        self.selection_fg
    }
}

#[derive(Deserialize)]
struct ThemeSnapshot {
    schema_version: u8,
    #[serde(rename = "name")]
    _name: String,
    palette: PaletteSnapshot,
}

#[derive(Deserialize)]
struct PaletteSnapshot {
    accent: WireColor,
    panel_bg: WireColor,
    surface0: WireColor,
    surface1: WireColor,
    surface_dim: WireColor,
    overlay0: WireColor,
    overlay1: WireColor,
    text: WireColor,
    subtext0: WireColor,
    mauve: WireColor,
    green: WireColor,
    yellow: WireColor,
    red: WireColor,
    blue: WireColor,
    teal: WireColor,
    peach: WireColor,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireColor {
    Reset,
    Ansi { name: String },
    Indexed { index: u8 },
    Rgb { r: u8, g: u8, b: u8 },
}

impl WireColor {
    fn into_color(self) -> Result<Color, PluginError> {
        match self {
            Self::Reset => Ok(Color::Reset),
            Self::Indexed { index } => Ok(Color::Indexed(index)),
            Self::Rgb { r, g, b } => Ok(Color::Rgb(r, g, b)),
            Self::Ansi { name } => match name.as_str() {
                "black" => Ok(Color::Black),
                "red" => Ok(Color::Red),
                "green" => Ok(Color::Green),
                "yellow" => Ok(Color::Yellow),
                "blue" => Ok(Color::Blue),
                "magenta" => Ok(Color::Magenta),
                "cyan" => Ok(Color::Cyan),
                "gray" => Ok(Color::Gray),
                "dark_gray" => Ok(Color::DarkGray),
                "light_red" => Ok(Color::LightRed),
                "light_green" => Ok(Color::LightGreen),
                "light_yellow" => Ok(Color::LightYellow),
                "light_blue" => Ok(Color::LightBlue),
                "light_magenta" => Ok(Color::LightMagenta),
                "light_cyan" => Ok(Color::LightCyan),
                "white" => Ok(Color::White),
                _ => Err(PluginError::new(format!(
                    "invalid {THEME_ENV_VAR}: unknown ANSI color {name:?}"
                ))),
            },
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn color(kind: &str) -> Value {
        json!({"kind": "ansi", "name": kind})
    }

    fn snapshot() -> Value {
        json!({
            "schema_version": 1,
            "name": "test-theme",
            "palette": {
                "accent": {"kind":"rgb", "r":1, "g":2, "b":3},
                "panel_bg": {"kind":"reset"},
                "surface0": {"kind":"indexed", "index":42},
                "surface1": color("black"),
                "surface_dim": color("dark_gray"),
                "overlay0": color("gray"),
                "overlay1": color("white"),
                "text": color("light_cyan"),
                "subtext0": color("cyan"),
                "mauve": color("magenta"),
                "green": color("green"),
                "yellow": color("yellow"),
                "red": color("red"),
                "blue": color("blue"),
                "teal": color("light_blue"),
                "peach": color("light_red")
            }
        })
    }

    #[test]
    fn parses_complete_lossless_palette_and_uses_herdr_contrast_rule() {
        let theme = PaneTheme::from_json(&snapshot().to_string()).unwrap();
        assert_eq!(theme.accent, Color::Rgb(1, 2, 3));
        assert_eq!(theme.panel_bg, Color::Reset);
        assert_eq!(theme.surface0, Color::Indexed(42));
        assert_eq!(theme.panel_contrast_fg(), Color::DarkGray);
        assert_eq!(theme.tab_selection().fg, Some(Color::DarkGray));
        assert_eq!(theme.tab_selection().bg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(theme.row_selection().fg, Some(Color::LightCyan));
        assert_eq!(theme.row_selection().bg, Some(Color::DarkGray));
        assert_eq!(theme.selection_band().fg, None);
        assert_eq!(theme.selection_band().bg, Some(Color::DarkGray));
    }

    #[test]
    fn resolved_theme_overrides_inherited_no_color_for_terminal_commands() {
        use ratatui::crossterm::style::{
            force_color_output, Color as CrosstermColor, SetForegroundColor,
        };
        use std::io::Write;

        // Model an agent process that disabled ANSI colors before Check-in started. This exercises
        // Crossterm's real command formatter rather than the TestBackend, which never observed the
        // production failure because it stores styles without serializing them.
        force_color_output(false);
        let theme = PaneTheme::from_json(&snapshot().to_string()).unwrap();
        theme.configure_terminal_output();

        let mut output = Vec::new();
        write!(output, "{}", SetForegroundColor(CrosstermColor::Cyan)).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(
            output.contains("\u{1b}[") && output.ends_with('m'),
            "resolved themes must emit an ANSI foreground command; got {output:?}"
        );
    }

    #[test]
    fn accepts_every_ansi_name() {
        for name in [
            "black",
            "red",
            "green",
            "yellow",
            "blue",
            "magenta",
            "cyan",
            "gray",
            "dark_gray",
            "light_red",
            "light_green",
            "light_yellow",
            "light_blue",
            "light_magenta",
            "light_cyan",
            "white",
        ] {
            let mut value = snapshot();
            value["palette"]["accent"] = color(name);
            assert!(PaneTheme::from_json(&value.to_string()).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_incomplete_unknown_or_future_snapshots() {
        let mut missing = snapshot();
        missing["palette"].as_object_mut().unwrap().remove("peach");
        assert!(PaneTheme::from_json(&missing.to_string()).is_err());

        let mut unknown = snapshot();
        unknown["palette"]["accent"] = color("orange");
        assert!(PaneTheme::from_json(&unknown.to_string()).is_err());

        let mut future = snapshot();
        future["schema_version"] = json!(2);
        assert!(PaneTheme::from_json(&future.to_string()).is_err());
    }

    #[test]
    fn ignores_forward_compatible_object_fields() {
        let mut value = snapshot();
        value["future"] = json!(true);
        value["palette"]["future_token"] = color("blue");
        assert!(PaneTheme::from_json(&value.to_string()).is_ok());
    }

    #[test]
    fn accepts_informational_empty_theme_name() {
        for name in ["", "   "] {
            let mut value = snapshot();
            value["name"] = json!(name);
            assert!(PaneTheme::from_json(&value.to_string()).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn legacy_palette_preserves_pre_theme_contract_styling() {
        let theme = PaneTheme::legacy();
        assert_eq!(theme.panel_bg, Color::Reset);
        assert_eq!(theme.text, Color::Reset);
        assert_eq!(theme.row_selection().fg, Some(Color::White));
        assert_eq!(theme.row_selection().bg, Some(Color::DarkGray));
        assert_eq!(theme.agent_identity().fg, Some(Color::Reset));
        assert_eq!(theme.tab_label().fg, Some(Color::Reset));
        assert_eq!(theme.pane_label().fg, Some(Color::Reset));
        assert_eq!(theme.status(theme.green).fg, Some(Color::DarkGray));
    }

    pub(crate) fn synthetic() -> PaneTheme {
        PaneTheme {
            accent: Color::Rgb(1, 2, 3),
            panel_bg: Color::Rgb(10, 11, 12),
            surface0: Color::Rgb(20, 21, 22),
            surface_dim: Color::Rgb(30, 31, 32),
            overlay0: Color::Rgb(40, 41, 42),
            overlay1: Color::Rgb(50, 51, 52),
            text: Color::Rgb(60, 61, 62),
            subtext0: Color::Rgb(70, 71, 72),
            mauve: Color::Rgb(90, 91, 92),
            green: Color::Rgb(100, 101, 102),
            yellow: Color::Rgb(80, 81, 82),
            red: Color::Rgb(110, 111, 112),
            teal: Color::Rgb(120, 121, 122),
            selection_fg: Color::Rgb(10, 11, 12),
            legacy: false,
        }
    }
}
