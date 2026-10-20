//! The Settings window as data (spec v1.1, section 6.1): every setting of the core and the
//! plugins from their JSON Schema, grouped by namespace, filtered by a search, edited by a
//! control its schema implies, written to the user or the project file. The settings files
//! stay the source of truth: the window writes them and reads back what the core reports.

use kumiko_core::config::{Layer, Setting, SettingKind};
use kumiko_core::protocol::methods::config::ConfigTarget;
use serde_json::Value;

use crate::input::LineInput;

/// A setting as the window shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: String,
    /// The namespace: `editor` of `editor.fontSize`.
    pub section: String,
    pub title: String,
    pub description: String,
    pub kind: SettingKind,
    pub value: Option<Value>,
    pub layer: Layer,
    /// Names of the choices, when they are not the values themselves (theme ids).
    pub labels: Vec<(Value, String)>,
}

impl Row {
    /// The value as the row shows it.
    pub fn display(&self) -> String {
        if let Some((_, label)) = self
            .labels
            .iter()
            .find(|(v, _)| Some(v) == self.value.as_ref())
        {
            return label.clone();
        }
        match &self.value {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        }
    }
}

/// What the window asks the workbench to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// Write the value (`null` removes the key, so the lower layer applies).
    Set(String, Value),
    /// Open the settings file of the target (structured values are edited there).
    OpenFile,
}

#[derive(Debug, Clone)]
pub struct SettingsDialog {
    pub search: LineInput,
    rows: Vec<Row>,
    /// Indices into `rows` that pass the search and the section.
    shown: Vec<usize>,
    pub section: Option<String>,
    /// Index into `shown`.
    pub selected: usize,
    /// The text of the selected text or number setting while it is edited.
    pub editing: Option<LineInput>,
    pub target: ConfigTarget,
    /// Why the last edit was not written.
    pub error: Option<String>,
}

impl SettingsDialog {
    /// `title` gives a translated title for a key or a section, if there is one.
    pub fn new(settings: Vec<Setting>, title: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut dialog = Self {
            search: LineInput::default(),
            rows: Vec::new(),
            shown: Vec::new(),
            section: None,
            selected: 0,
            editing: None,
            target: ConfigTarget::User,
            error: None,
        };
        dialog.replace(settings, title);
        dialog
    }

    /// New values from the core (after `config.changed`); the selection stays on its key.
    pub fn replace(&mut self, settings: Vec<Setting>, title: &dyn Fn(&str) -> Option<String>) {
        let current = self.current().map(|row| row.key.clone());
        self.rows = settings
            .into_iter()
            .map(|setting| {
                let section = setting
                    .key
                    .split_once('.')
                    .map_or_else(|| setting.key.clone(), |(s, _)| s.to_owned());
                let title = title(&setting.key)
                    .or_else(|| setting.schema.title().map(str::to_owned))
                    .unwrap_or_else(|| humanize(&setting.key));
                Row {
                    title,
                    section,
                    description: setting.schema.description().unwrap_or_default().to_owned(),
                    kind: setting.schema.kind(),
                    value: setting.value,
                    layer: setting.layer,
                    labels: Vec::new(),
                    key: setting.key,
                }
            })
            .collect();
        self.filter();
        if let Some(key) = current
            && let Some(at) = self.shown.iter().position(|&i| self.rows[i].key == key)
        {
            self.selected = at;
        }
    }

    /// Makes a setting a choice among `choices` (value, name): installed themes for a
    /// theme id the schema leaves as text.
    pub fn set_choices(&mut self, key: &str, choices: Vec<(Value, String)>) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.key == key) {
            row.kind = SettingKind::Choice(choices.iter().map(|(v, _)| v.clone()).collect());
            row.labels = choices;
        }
    }

    /// Sections in the order of their first setting (keys are sorted).
    pub fn sections(&self) -> Vec<String> {
        let mut sections: Vec<String> = Vec::new();
        for row in &self.rows {
            if sections.last() != Some(&row.section) {
                sections.push(row.section.clone());
            }
        }
        sections
    }

    pub fn shown(&self) -> impl Iterator<Item = &Row> {
        self.shown.iter().map(|&i| &self.rows[i])
    }

    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    pub fn current(&self) -> Option<&Row> {
        self.shown.get(self.selected).map(|&i| &self.rows[i])
    }

    /// Applies the search text and the section; the search looks at keys, titles and
    /// descriptions, ignoring case.
    pub fn filter(&mut self) {
        let query = self.search.text().to_lowercase();
        self.shown = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.section.as_ref().is_none_or(|s| *s == row.section))
            .filter(|(_, row)| {
                query.is_empty()
                    || [&row.key, &row.title, &row.description]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&query))
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
        self.editing = None;
    }

    pub fn set_section(&mut self, section: Option<String>) {
        self.section = section;
        self.selected = 0;
        self.filter();
    }

    pub fn select(&mut self, index: usize) {
        if index < self.shown.len() {
            self.selected = index;
            self.editing = None;
            self.error = None;
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last) as usize;
        self.select(next);
    }

    pub fn toggle_target(&mut self) {
        self.target = match self.target {
            ConfigTarget::User => ConfigTarget::Workspace,
            ConfigTarget::Workspace => ConfigTarget::User,
        };
    }

    /// Enter on the selected setting: flip a switch, step a choice, start or finish editing
    /// text, open the file for structured values.
    pub fn activate(&mut self) -> Option<Change> {
        if self.editing.is_some() {
            return self.commit();
        }
        let row = self.current()?.clone();
        match &row.kind {
            SettingKind::Boolean => {
                let on = row.value.as_ref().and_then(Value::as_bool).unwrap_or(false);
                Some(Change::Set(row.key, Value::Bool(!on)))
            }
            SettingKind::Choice(_) => self.step(1),
            SettingKind::Number { .. } | SettingKind::Text { .. } => {
                let mut input = LineInput::with_text(&row.display());
                input.select_all();
                self.editing = Some(input);
                self.error = None;
                None
            }
            SettingKind::Structured => Some(Change::OpenFile),
        }
    }

    /// Left and right on a choice: the previous or next allowed value.
    pub fn step(&mut self, delta: isize) -> Option<Change> {
        let row = self.current()?;
        let SettingKind::Choice(values) = &row.kind else {
            return None;
        };
        if values.is_empty() {
            return None;
        }
        let at = row
            .value
            .as_ref()
            .and_then(|v| values.iter().position(|x| x == v))
            .unwrap_or(0) as isize;
        let count = values.len() as isize;
        let next = &values[(at + delta).rem_euclid(count) as usize];
        // A single choice stays as it is rather than being written down.
        (row.value.as_ref() != Some(next)).then(|| Change::Set(row.key.clone(), next.clone()))
    }

    /// The edited text as the setting's value; `None` and an error when it does not fit.
    pub fn commit(&mut self) -> Option<Change> {
        let input = self.editing.take()?;
        let row = self.current()?.clone();
        let text = input.text().trim();
        let value = match &row.kind {
            SettingKind::Number {
                integer,
                minimum,
                maximum,
            } => match parse_number(text, *integer, *minimum, *maximum) {
                Ok(value) => value,
                Err(error) => {
                    self.error = Some(error);
                    self.editing = Some(input);
                    return None;
                }
            },
            SettingKind::Text { nullable: true } if text.is_empty() => Value::Null,
            _ => Value::String(text.to_owned()),
        };
        self.error = None;
        Some(Change::Set(row.key, value))
    }

    pub fn cancel_edit(&mut self) -> bool {
        self.error = None;
        self.editing.take().is_some()
    }

    /// Removes the value from the target file, so the layer below applies.
    pub fn reset(&mut self) -> Option<Change> {
        let row = self.current()?;
        let layer = match self.target {
            ConfigTarget::User => Layer::User,
            ConfigTarget::Workspace => Layer::Workspace,
        };
        (row.layer == layer).then(|| Change::Set(row.key.clone(), Value::Null))
    }
}

fn parse_number(
    text: &str,
    integer: bool,
    minimum: Option<f64>,
    maximum: Option<f64>,
) -> Result<Value, String> {
    let value: Value = if integer {
        text.parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("`{text}` is not a whole number"))?
    } else {
        let number = text
            .parse::<f64>()
            .map_err(|_| format!("`{text}` is not a number"))?;
        serde_json::Number::from_f64(number)
            .map(Value::Number)
            .ok_or_else(|| format!("`{text}` is not a number"))?
    };
    let number = value.as_f64().unwrap_or_default();
    if let Some(min) = minimum
        && number < min
    {
        return Err(format!("at least {min}"));
    }
    if let Some(max) = maximum
        && number > max
    {
        return Err(format!("at most {max}"));
    }
    Ok(value)
}

/// `editor.fontSize` → `Font size`, `files.eol` → `Eol`: the last part, words split at
/// capitals.
pub fn humanize(key: &str) -> String {
    let last = key.rsplit('.').next().unwrap_or(key);
    let mut words = String::new();
    for (i, c) in last.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            words.push(' ');
            words.extend(c.to_lowercase());
        } else if i == 0 {
            words.extend(c.to_uppercase());
        } else {
            words.push(c);
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use kumiko_core::config::PropertySchema;
    use serde_json::json;

    fn setting(key: &str, schema: Value, value: Option<Value>, layer: Layer) -> Setting {
        Setting {
            key: key.into(),
            owner: "core".into(),
            schema: PropertySchema::new(schema).unwrap(),
            value,
            layer,
        }
    }

    fn dialog() -> SettingsDialog {
        let settings = vec![
            setting(
                "editor.fontSize",
                json!({"type": "integer", "minimum": 6, "maximum": 72, "description": "Points"}),
                Some(json!(14)),
                Layer::Default,
            ),
            setting(
                "editor.softWrap",
                json!({"type": "boolean"}),
                Some(json!(false)),
                Layer::User,
            ),
            setting(
                "files.eol",
                json!({"enum": ["auto", "lf", "crlf"]}),
                Some(json!("lf")),
                Layer::Workspace,
            ),
            setting(
                "files.exclude",
                json!({"type": "array"}),
                Some(json!([])),
                Layer::Default,
            ),
            setting(
                "workbench.locale",
                json!({"type": ["string", "null"]}),
                None,
                Layer::Default,
            ),
        ];
        let titles = |key: &str| (key == "editor.fontSize").then(|| "Размер шрифта".to_owned());
        SettingsDialog::new(settings, &titles)
    }

    #[test]
    fn titles_sections_and_search() {
        let mut d = dialog();
        assert_eq!(d.sections(), ["editor", "files", "workbench"]);
        let titles: Vec<&str> = d.shown().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Размер шрифта", "Soft wrap", "Eol", "Exclude", "Locale"]
        );

        d.search = LineInput::with_text("POINTS");
        d.filter();
        assert_eq!(
            d.shown().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["editor.fontSize"]
        );

        d.search = LineInput::default();
        d.set_section(Some("files".into()));
        assert_eq!(d.shown_len(), 2);
        assert_eq!(d.current().unwrap().key, "files.eol");
    }

    #[test]
    fn controls_follow_the_schema() {
        let mut d = dialog();
        // Number: edited as text, checked against the schema.
        assert_eq!(d.activate(), None);
        d.editing = Some(LineInput::with_text("3"));
        assert_eq!(d.activate(), None, "below the minimum");
        assert_eq!(d.error.as_deref(), Some("at least 6"));
        d.editing = Some(LineInput::with_text("16"));
        assert_eq!(
            d.activate(),
            Some(Change::Set("editor.fontSize".into(), json!(16)))
        );

        d.move_selection(1);
        assert_eq!(
            d.activate(),
            Some(Change::Set("editor.softWrap".into(), json!(true)))
        );

        d.move_selection(1);
        assert_eq!(
            d.step(1),
            Some(Change::Set("files.eol".into(), json!("crlf")))
        );
        assert_eq!(
            d.step(-2),
            Some(Change::Set("files.eol".into(), json!("crlf")))
        );

        d.set_choices("files.eol", vec![(json!("lf"), "LF".into())]);
        assert_eq!(d.step(1), None, "the only choice is already set");
        assert_eq!(d.current().unwrap().display(), "LF");

        d.move_selection(1);
        assert_eq!(d.activate(), Some(Change::OpenFile));

        // A nullable text: empty means null.
        d.move_selection(1);
        d.activate();
        d.editing = Some(LineInput::default());
        assert_eq!(
            d.activate(),
            Some(Change::Set("workbench.locale".into(), Value::Null))
        );
    }

    #[test]
    fn reset_removes_only_from_the_target_layer() {
        let mut d = dialog();
        d.move_selection(1); // softWrap, set by the user
        assert_eq!(
            d.reset(),
            Some(Change::Set("editor.softWrap".into(), Value::Null))
        );
        d.toggle_target();
        assert_eq!(d.reset(), None, "the project file does not set it");
        d.move_selection(1); // eol, set by the project
        assert_eq!(
            d.reset(),
            Some(Change::Set("files.eol".into(), Value::Null))
        );
    }

    #[test]
    fn replacing_keeps_the_selected_key() {
        let mut d = dialog();
        d.move_selection(2);
        let fewer = vec![
            setting(
                "editor.softWrap",
                json!({"type": "boolean"}),
                None,
                Layer::Default,
            ),
            setting("files.eol", json!({"enum": ["lf"]}), None, Layer::Default),
        ];
        d.replace(fewer, &|_| None);
        assert_eq!(d.current().unwrap().key, "files.eol");
    }

    #[test]
    fn humanized_keys() {
        assert_eq!(humanize("editor.fontSize"), "Font size");
        assert_eq!(humanize("java.jdtls.path"), "Path");
        assert_eq!(humanize("x"), "X");
    }
}
