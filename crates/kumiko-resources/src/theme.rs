//! Themes: UI color tokens and syntax capture styles (spec, section 21).
//!
//! A missing token is not an error: lookups walk `extends`, then dotted capture parents
//! (`keyword.control` → `keyword`); what is still missing falls back to the UI host's
//! defaults for the theme's base.

use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeBase {
    Dark,
    Light,
    HighContrast,
}

/// `#RRGGBB` or `#RRGGBBAA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: if hex.len() == 8 { byte(6)? } else { 255 },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyntaxStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub id: String,
    pub base: ThemeBase,
    pub extends: Option<String>,
    colors: BTreeMap<String, Color>,
    syntax: BTreeMap<String, SyntaxStyle>,
    /// Invalid entries skipped while parsing.
    pub problems: Vec<String>,
}

impl Theme {
    pub(crate) fn from_manifest(id: &str, manifest: &Value) -> Result<Self, String> {
        let header = manifest.get("theme").ok_or("missing [theme] table")?;
        let base = match header.get("base").and_then(Value::as_str) {
            Some("dark") => ThemeBase::Dark,
            Some("light") => ThemeBase::Light,
            Some("high-contrast") => ThemeBase::HighContrast,
            other => {
                return Err(format!(
                    "`base` must be dark, light or high-contrast, got {other:?}"
                ));
            }
        };
        let mut problems = Vec::new();
        let mut colors = BTreeMap::new();
        for (token, value) in manifest
            .get("colors")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match value.as_str().and_then(Color::parse) {
                Some(color) => {
                    colors.insert(token.clone(), color);
                }
                None => problems.push(format!("color `{token}`: expected #RRGGBB, got {value}")),
            }
        }
        let mut syntax = BTreeMap::new();
        for (capture, value) in manifest
            .get("syntax")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match parse_style(value) {
                Ok(style) => {
                    syntax.insert(capture.clone(), style);
                }
                Err(message) => problems.push(format!("syntax `{capture}`: {message}")),
            }
        }
        Ok(Self {
            id: id.to_owned(),
            base,
            extends: header
                .get("extends")
                .and_then(Value::as_str)
                .map(str::to_owned),
            colors,
            syntax,
            problems,
        })
    }

    /// Fills tokens missing here from a parent theme.
    pub(crate) fn inherit(&mut self, parent: &Theme) {
        for (token, color) in &parent.colors {
            self.colors.entry(token.clone()).or_insert(*color);
        }
        for (capture, style) in &parent.syntax {
            self.syntax.entry(capture.clone()).or_insert(*style);
        }
    }

    pub fn color(&self, token: &str) -> Option<Color> {
        self.colors.get(token).copied()
    }

    /// Every syntax style the theme (with its parents) defines, by capture name.
    pub fn syntax_styles(&self) -> Vec<(String, SyntaxStyle)> {
        self.syntax
            .iter()
            .map(|(capture, style)| (capture.clone(), *style))
            .collect()
    }

    /// Style of a capture, falling back to its dotted parents.
    pub fn syntax(&self, capture: &str) -> Option<SyntaxStyle> {
        let mut name = capture;
        loop {
            if let Some(style) = self.syntax.get(name) {
                return Some(*style);
            }
            name = &name[..name.rfind('.')?];
        }
    }
}

fn parse_style(value: &Value) -> Result<SyntaxStyle, String> {
    let color = |v: Option<&Value>| -> Result<Option<Color>, String> {
        match v {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .and_then(Color::parse)
                .map(Some)
                .ok_or_else(|| format!("expected #RRGGBB, got {v}")),
        }
    };
    match value {
        Value::String(_) => Ok(SyntaxStyle {
            fg: color(Some(value))?,
            ..SyntaxStyle::default()
        }),
        Value::Object(table) => {
            let flag = |name: &str| table.get(name).and_then(Value::as_bool).unwrap_or(false);
            Ok(SyntaxStyle {
                fg: color(table.get("fg"))?,
                bg: color(table.get("bg"))?,
                bold: flag("bold"),
                italic: flag("italic"),
                underline: flag("underline"),
            })
        }
        other => Err(format!("expected a color or a table, got {other}")),
    }
}
