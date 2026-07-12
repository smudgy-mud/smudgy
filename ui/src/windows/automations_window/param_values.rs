//! The reusable rich value-editor for package parameters.
//!
//! A package declares parameters (`smudgy.package.json`'s `params`) and the user configures their
//! values at install time and afterwards. The two value-entry surfaces — the install-time
//! [`ParamPrompt`](super::packages::ParamPrompt) gate and the in-pane
//! [`ParamConfig`](super::packages::ParamConfig) editor — share this module so a parameter renders
//! and round-trips identically wherever it's edited.
//!
//! Each parameter's in-progress value is held as a [`ParamValueState`] (a checkbox bool, a dropdown
//! selection, a number's text buffer, a list of elements, a table of rows…). [`seed`] builds that
//! state from the stored JSON, [`view`] renders the matching control(s), [`apply`] folds one edit
//! into the state, and [`to_json`] projects it back to the JSON value persisted to disk (and read
//! by `smudgy:params`). Scalars are stored as a JSON scalar; a `List` as a JSON array of scalars; a
//! `Table` as a JSON array of per-row objects keyed by column.

use iced::alignment::Vertical;
use iced::widget::{Column, button, checkbox, container, pick_list, row, text, text_input};
use iced::{Length, Padding};
use serde_json::Value;

use smudgy_core::models::shared_packages::{PackageParameter, ParamKind};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::builtins::button as button_style;

use super::common;
use super::{Elem, Message};

/// Label-column width for a scalar field row (matches the install/config panes' framing).
const LABEL_WIDTH: f32 = 140.0;

/// Which value-entry surface an edit belongs to, so the one shared [`Message::ParamValueEdit`]
/// routes to the right handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamTarget {
    /// The persistent in-pane editor ([`super::packages::ParamConfig`]).
    Config,
    /// The install-time required-params gate ([`super::packages::ParamPrompt`]).
    Prompt,
}

/// The in-progress edit state of one parameter's value. Scalars hold a single value; containers hold
/// their sub-values, each itself a *scalar* state (containers never nest).
#[derive(Debug, Clone)]
pub enum ParamValueState {
    /// A free-text buffer — a `String`/secret value, or a `Number`'s text (parsed on save so a
    /// half-typed `-` / `1.` is allowed mid-edit).
    Text(String),
    /// A boolean, edited with a checkbox.
    Bool(bool),
    /// A `Dropdown` selection: the chosen option's stored value, or `None` when nothing is picked.
    Choice(Option<String>),
    /// A `List`'s elements, in order — each a scalar state matching the element spec.
    List(Vec<ParamValueState>),
    /// A `Table`'s rows; each row holds one scalar state per column, in column order.
    Table(Vec<Vec<ParamValueState>>),
}

/// An edit to a single *scalar* value (a top-level scalar param, a list element, or a table cell).
#[derive(Debug, Clone)]
pub enum ScalarEdit {
    /// Replace a text/number buffer.
    Text(String),
    /// Set a boolean.
    Bool(bool),
    /// Choose a dropdown option by its stored value (the empty string clears the selection).
    Choice(String),
}

/// One edit addressed within a single parameter's [`ParamValueState`]. The parameter key travels
/// alongside in [`Message::ParamValueEdit`]; this enum says *where inside that parameter* to apply it.
#[derive(Debug, Clone)]
pub enum ParamValueEdit {
    /// Edit the top-level scalar value (string/secret/number/bool/dropdown).
    Scalar(ScalarEdit),
    /// Append a fresh element to a list.
    ListAdd,
    /// Insert a fresh element above index `n`.
    ListInsert(usize),
    /// Remove the element at index `n`.
    ListRemove(usize),
    /// Edit the list element at index `n`.
    ListSet(usize, ScalarEdit),
    /// Append a fresh row to a table.
    TableAddRow,
    /// Insert a fresh row above row `n`.
    TableInsertRow(usize),
    /// Remove the row at index `n`.
    TableRemoveRow(usize),
    /// Edit the cell at (`row`, `col`) of a table.
    TableSet(usize, usize, ScalarEdit),
}

// ============================================================================
// Seed (stored JSON -> editing state)
// ============================================================================

/// Build the editing state for `spec` from its currently-stored JSON value (or `None` when unset).
/// Containers seed exactly one element per stored entry / one cell per declared column, so the
/// rendered grid always matches the live manifest even if the stored value predates a column change.
#[must_use]
pub fn seed(spec: &PackageParameter, stored: Option<&Value>) -> ParamValueState {
    match spec.kind {
        ParamKind::Bool => ParamValueState::Bool(
            stored
                .and_then(Value::as_bool)
                .or_else(|| spec.default.as_ref().and_then(Value::as_bool))
                .unwrap_or(false),
        ),
        ParamKind::Dropdown => ParamValueState::Choice(seed_choice(spec, stored)),
        ParamKind::List => {
            let element = spec.fields.first();
            let items = stored
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|v| seed_scalar(element, Some(v)))
                        .collect()
                })
                .unwrap_or_default();
            ParamValueState::List(items)
        }
        ParamKind::Table => {
            let rows = stored
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .map(|row_val| {
                            spec.fields
                                .iter()
                                .map(|col| seed_scalar(Some(col), row_val.get(&col.key)))
                                .collect()
                        })
                        .collect()
                })
                .unwrap_or_default();
            ParamValueState::Table(rows)
        }
        // String / Number, and any other scalar.
        _ => ParamValueState::Text(stored.map(scalar_text).unwrap_or_default()),
    }
}

/// Seed the state of one scalar sub-value (a list element or table cell) from its stored JSON. A
/// `None` element spec (a malformed manifest) or a non-scalar spec falls back to a text buffer.
fn seed_scalar(spec: Option<&PackageParameter>, stored: Option<&Value>) -> ParamValueState {
    match spec.map(|s| s.kind) {
        Some(ParamKind::Bool) => {
            ParamValueState::Bool(stored.and_then(Value::as_bool).unwrap_or(false))
        }
        Some(ParamKind::Dropdown) => {
            ParamValueState::Choice(seed_choice(spec.unwrap(), stored))
        }
        _ => ParamValueState::Text(stored.map(scalar_text).unwrap_or_default()),
    }
}

/// The dropdown selection to seed: the stored (then default) value, kept only when it's still a
/// declared option so a removed option doesn't linger as a phantom selection.
fn seed_choice(spec: &PackageParameter, stored: Option<&Value>) -> Option<String> {
    stored
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.default.as_ref().and_then(Value::as_str).map(str::to_string))
        .filter(|v| spec.options.iter().any(|o| &o.value == v))
}

/// Render a stored JSON scalar back to editable text (strings verbatim, other scalars as their
/// literal form, null as empty).
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ============================================================================
// Project (editing state -> stored JSON)
// ============================================================================

/// Project `state` back to the JSON value to persist for `spec`. `Ok(None)` means "unset" (the
/// caller clears any stored value); `Ok(Some(_))` is the value to store; `Err` is a validation
/// failure (a number that doesn't parse, a dropdown value that isn't a declared option). Required-ness
/// is *not* enforced here — the caller treats `Ok(None)` for a required param as the error.
///
/// Empty entries are pruned: a blank list element / fully-blank table row is dropped, and an
/// emptied list/table collapses to `Ok(None)`. A boolean always yields a value (a checkbox has no
/// "unset").
pub fn to_json(spec: &PackageParameter, state: &ParamValueState) -> Result<Option<Value>, String> {
    match (spec.kind, state) {
        (ParamKind::List, ParamValueState::List(items)) => {
            let element = spec.fields.first();
            let mut out = Vec::new();
            for item in items {
                if let Some(value) = scalar_to_json(element, item)? {
                    out.push(value);
                }
            }
            Ok((!out.is_empty()).then(|| Value::Array(out)))
        }
        (ParamKind::Table, ParamValueState::Table(rows)) => {
            let mut out = Vec::new();
            for row_state in rows {
                let mut object = serde_json::Map::new();
                // Only non-empty cells are stored, so a row object's values stay within the
                // `Record<string, ParamScalar>` shape the `smudgy:params` typings promise (a blank
                // cell is an absent key, read back as `undefined`, never a JSON `null`). `seed`
                // already treats an absent key the same as a stored null, so this round-trips.
                for (col, cell) in spec.fields.iter().zip(row_state) {
                    if let Some(value) = scalar_to_json(Some(col), cell)? {
                        object.insert(col.key.clone(), value);
                    }
                }
                // A fully-blank row contributes no keys and is dropped.
                if !object.is_empty() {
                    out.push(Value::Object(object));
                }
            }
            Ok((!out.is_empty()).then(|| Value::Array(out)))
        }
        // Scalars (and a state/kind mismatch from a hand-broken manifest).
        _ => scalar_to_json(Some(spec), state),
    }
}

/// Project one scalar sub-value to JSON. `Ok(None)` is an empty/unset cell.
fn scalar_to_json(
    spec: Option<&PackageParameter>,
    state: &ParamValueState,
) -> Result<Option<Value>, String> {
    match state {
        ParamValueState::Bool(b) => Ok(Some(Value::Bool(*b))),
        ParamValueState::Choice(choice) => match choice {
            None => Ok(None),
            Some(value) => {
                if let Some(spec) = spec
                    && !spec.options.is_empty()
                    && !spec.options.iter().any(|o| &o.value == value)
                {
                    return Err(format!("\u{201c}{value}\u{201d} is not one of the choices."));
                }
                Ok(Some(Value::String(value.clone())))
            }
        },
        ParamValueState::Text(raw) => {
            if spec.is_some_and(|s| s.kind == ParamKind::Number) {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    parse_number(trimmed).map(Some)
                }
            } else {
                let trimmed = raw.trim();
                Ok((!trimmed.is_empty()).then(|| Value::String(trimmed.to_string())))
            }
        }
        // A container nested where a scalar was expected can't be produced by the editor.
        ParamValueState::List(_) | ParamValueState::Table(_) => Ok(None),
    }
}

/// Parse a numeric text buffer to a JSON number, mirroring `serde_json`'s own integer widening
/// (`i64`, then `u64`, then `f64`) so any integer round-trips exactly. Shared with the manifest
/// editor's default parsing.
pub(super) fn parse_number(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if let Ok(int) = text.parse::<i64>() {
        Ok(Value::Number(int.into()))
    } else if let Ok(uint) = text.parse::<u64>() {
        Ok(Value::Number(uint.into()))
    } else if let Some(num) = text.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
        Ok(Value::Number(num))
    } else {
        Err("must be a number.".to_string())
    }
}

// ============================================================================
// Apply (fold one edit into the state)
// ============================================================================

/// Fold one [`ParamValueEdit`] into `state`. A list/table op that targets a stale index is a no-op
/// (the index is re-validated against the current length).
pub fn apply(spec: &PackageParameter, state: &mut ParamValueState, edit: ParamValueEdit) {
    match edit {
        ParamValueEdit::Scalar(scalar) => apply_scalar(state, scalar),
        ParamValueEdit::ListAdd => {
            if let ParamValueState::List(items) = state {
                items.push(fresh_element(spec));
            }
        }
        ParamValueEdit::ListInsert(at) => {
            if let ParamValueState::List(items) = state {
                let at = at.min(items.len());
                items.insert(at, fresh_element(spec));
            }
        }
        ParamValueEdit::ListRemove(at) => {
            if let ParamValueState::List(items) = state
                && at < items.len()
            {
                items.remove(at);
            }
        }
        ParamValueEdit::ListSet(at, scalar) => {
            if let ParamValueState::List(items) = state
                && let Some(slot) = items.get_mut(at)
            {
                apply_scalar(slot, scalar);
            }
        }
        ParamValueEdit::TableAddRow => {
            if let ParamValueState::Table(rows) = state {
                rows.push(fresh_row(spec));
            }
        }
        ParamValueEdit::TableInsertRow(at) => {
            if let ParamValueState::Table(rows) = state {
                let at = at.min(rows.len());
                rows.insert(at, fresh_row(spec));
            }
        }
        ParamValueEdit::TableRemoveRow(at) => {
            if let ParamValueState::Table(rows) = state
                && at < rows.len()
            {
                rows.remove(at);
            }
        }
        ParamValueEdit::TableSet(r, c, scalar) => {
            if let ParamValueState::Table(rows) = state
                && let Some(cell) = rows.get_mut(r).and_then(|row| row.get_mut(c))
            {
                apply_scalar(cell, scalar);
            }
        }
    }
}

/// Apply a scalar edit to a scalar state. A mismatched pair (which the controls never emit) is a
/// no-op.
fn apply_scalar(state: &mut ParamValueState, edit: ScalarEdit) {
    match (state, edit) {
        (ParamValueState::Text(buffer), ScalarEdit::Text(value)) => *buffer = value,
        (ParamValueState::Bool(flag), ScalarEdit::Bool(value)) => *flag = value,
        (ParamValueState::Choice(choice), ScalarEdit::Choice(value)) => {
            *choice = (!value.is_empty()).then_some(value);
        }
        _ => {}
    }
}

/// A fresh, empty state for a list element (honoring a scalar default for bool/dropdown).
fn fresh_element(spec: &PackageParameter) -> ParamValueState {
    spec.fields.first().map_or(ParamValueState::Text(String::new()), fresh_scalar)
}

/// A fresh, empty row: one fresh scalar per declared column.
fn fresh_row(spec: &PackageParameter) -> Vec<ParamValueState> {
    spec.fields.iter().map(fresh_scalar).collect()
}

/// A fresh, empty scalar state for `spec`'s kind (a bool/dropdown honoring its declared default).
fn fresh_scalar(spec: &PackageParameter) -> ParamValueState {
    match spec.kind {
        ParamKind::Bool => {
            ParamValueState::Bool(spec.default.as_ref().and_then(Value::as_bool).unwrap_or(false))
        }
        ParamKind::Dropdown => ParamValueState::Choice(
            spec.default
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|v| spec.options.iter().any(|o| &o.value == v)),
        ),
        _ => ParamValueState::Text(String::new()),
    }
}

// ============================================================================
// View (render the control(s) for a non-secret parameter)
// ============================================================================

/// Where a scalar control routes its edit: the top-level value, a list element, or a table cell.
#[derive(Debug, Clone, Copy)]
enum Sink {
    Top,
    List(usize),
    Table(usize, usize),
}

impl Sink {
    /// Wrap a scalar edit into the addressed [`ParamValueEdit`].
    fn wrap(self, edit: ScalarEdit) -> ParamValueEdit {
        match self {
            Sink::Top => ParamValueEdit::Scalar(edit),
            Sink::List(i) => ParamValueEdit::ListSet(i, edit),
            Sink::Table(r, c) => ParamValueEdit::TableSet(r, c, edit),
        }
    }
}

/// Build the routed edit message for `key` on `target`.
fn edit_msg(target: ParamTarget, key: &str, edit: ParamValueEdit) -> Message {
    Message::ParamValueEdit(target, key.to_string(), edit)
}

/// Render the complete field (label + control) for one **non-secret** parameter. Secret params are
/// rendered by the caller (a secure text box), since they aren't read back into the editor.
pub fn view<'a>(
    spec: &'a PackageParameter,
    state: &'a ParamValueState,
    target: ParamTarget,
) -> Elem<'a> {
    let mut label = spec.label.as_deref().unwrap_or(&spec.key).to_string();
    if spec.required {
        label.push_str(" *");
    }
    match (spec.kind, state) {
        (ParamKind::List, ParamValueState::List(items)) => list_block(&label, spec, items, target),
        (ParamKind::Table, ParamValueState::Table(rows)) => table_block(&label, spec, rows, target),
        _ => labelled_row(label, scalar_control(spec, state, target, &spec.key, Sink::Top)),
    }
}

/// A scalar field laid out as a left label column + the control.
fn labelled_row<'a>(label: String, control: Elem<'a>) -> Elem<'a> {
    row![
        container(text(label).size(13.0)).width(Length::Fixed(LABEL_WIDTH)),
        control,
    ]
    .spacing(8.0)
    .align_y(Vertical::Center)
    .into()
}

/// The control for one scalar value (top-level, list element, or table cell): a checkbox for a
/// bool, a pick list for a dropdown, a text input otherwise. `cell` is the scalar's own spec
/// (its kind/options/default); `sink` says where the edit is routed.
fn scalar_control<'a>(
    cell: &'a PackageParameter,
    state: &'a ParamValueState,
    target: ParamTarget,
    key: &str,
    sink: Sink,
) -> Elem<'a> {
    match (cell.kind, state) {
        (ParamKind::Bool, ParamValueState::Bool(flag)) => {
            let key = key.to_string();
            checkbox(*flag)
                .size(18)
                .on_toggle(move |v| edit_msg(target, &key, sink.wrap(ScalarEdit::Bool(v))))
                .into()
        }
        (ParamKind::Dropdown, ParamValueState::Choice(choice)) => {
            choice_control(cell, choice.as_deref(), target, key, sink)
        }
        _ => {
            let key = key.to_string();
            let buffer = match state {
                ParamValueState::Text(buffer) => buffer.as_str(),
                _ => "",
            };
            text_input(&scalar_placeholder(cell, sink), buffer)
                .on_input(move |v| edit_msg(target, &key, sink.wrap(ScalarEdit::Text(v))))
                .size(14.0)
                .width(Length::Fill)
                .into()
        }
    }
}

/// The placeholder for a scalar text/number input. A top-level field surfaces the manifest default
/// as a hint; a list element / table cell stays terse (the column header already names it).
fn scalar_placeholder(cell: &PackageParameter, sink: Sink) -> String {
    if let Sink::Top = sink
        && let Some(default) = &cell.default
    {
        return format!("default: {}", scalar_text(default));
    }
    match cell.kind {
        ParamKind::Number => "number".to_string(),
        _ => "value".to_string(),
    }
}

/// A pick list over a dropdown's declared options, plus a leading "(none)" entry whenever the field
/// may be left unset (any non-required top-level field, and every list/table cell).
fn choice_control<'a>(
    cell: &PackageParameter,
    current: Option<&str>,
    target: ParamTarget,
    key: &str,
    sink: Sink,
) -> Elem<'a> {
    let clearable = !matches!(sink, Sink::Top) || !cell.required;
    let mut choices = Vec::with_capacity(cell.options.len() + usize::from(clearable));
    if clearable {
        choices.push(Choice { value: String::new(), label: "(none)".to_string() });
    }
    for option in &cell.options {
        choices.push(Choice {
            value: option.value.clone(),
            label: option.display_label().to_string(),
        });
    }
    let selected = current
        .and_then(|value| choices.iter().find(|c| c.value == value).cloned())
        .or_else(|| clearable.then(|| choices[0].clone()));
    let key = key.to_string();
    pick_list(choices, selected, move |choice: Choice| {
        edit_msg(target, &key, sink.wrap(ScalarEdit::Choice(choice.value)))
    })
    .text_size(13.0)
    .width(Length::Fill)
    .into()
}

/// A `pick_list`-friendly dropdown entry. An empty `value` is the "(none)" sentinel that clears the
/// selection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    value: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// A `List` block: its label, one element row per entry (control + insert-above + remove), and an
/// add button.
fn list_block<'a>(
    label: &str,
    spec: &'a PackageParameter,
    items: &'a [ParamValueState],
    target: ParamTarget,
) -> Elem<'a> {
    let element = spec.fields.first();
    let mut col = Column::new().spacing(6.0).push(text(label.to_string()).size(13.0));
    if items.is_empty() {
        col = col.push(text("No entries.").size(12.0).style(common::faint));
    }
    for (i, item) in items.iter().enumerate() {
        let control: Elem<'a> = match element {
            Some(element) => scalar_control(element, item, target, &spec.key, Sink::List(i)),
            None => text("(no element type declared)").size(12.0).style(common::faint).into(),
        };
        col = col.push(
            row![
                container(control).width(Length::Fill),
                row_icon_button(
                    bootstrap_icons::CHEVRON_UP,
                    edit_msg(target, &spec.key, ParamValueEdit::ListInsert(i)),
                ),
                row_icon_button(
                    bootstrap_icons::TRASH_3,
                    edit_msg(target, &spec.key, ParamValueEdit::ListRemove(i)),
                ),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
        );
    }
    col = col.push(add_button(
        "Add entry",
        edit_msg(target, &spec.key, ParamValueEdit::ListAdd),
    ));
    container(col).padding(12.0).width(Length::Fill).style(common::banner_style).into()
}

/// A `Table` block: its label, a column-header row, one row per data row (a cell control per column
/// + insert-above + remove), and an add-row button.
fn table_block<'a>(
    label: &str,
    spec: &'a PackageParameter,
    rows: &'a [Vec<ParamValueState>],
    target: ParamTarget,
) -> Elem<'a> {
    let mut col = Column::new().spacing(6.0).push(text(label.to_string()).size(13.0));

    if spec.fields.is_empty() {
        col = col.push(text("No columns declared.").size(12.0).style(common::faint));
        return container(col).padding(12.0).width(Length::Fill).style(common::banner_style).into();
    }

    // Column headers, aligned with the per-row cells; a trailing gap reserves the action buttons'
    // width so the header labels line up over their columns.
    let mut header = row![].spacing(8.0).align_y(Vertical::Center);
    for field in &spec.fields {
        let name = field.label.as_deref().unwrap_or(&field.key);
        header = header.push(
            container(text(name.to_string()).size(11.0).font(fonts::GEIST_VF).style(common::muted))
                .width(Length::Fill),
        );
    }
    header = header.push(container(text("")).width(Length::Fixed(ROW_ACTIONS_WIDTH)));
    col = col.push(header);

    if rows.is_empty() {
        col = col.push(text("No rows.").size(12.0).style(common::faint));
    }
    for (r, row_state) in rows.iter().enumerate() {
        let mut cells = row![].spacing(8.0).align_y(Vertical::Center);
        for (c, field) in spec.fields.iter().enumerate() {
            let control: Elem<'a> = match row_state.get(c) {
                Some(cell) => scalar_control(field, cell, target, &spec.key, Sink::Table(r, c)),
                None => text("").into(),
            };
            cells = cells.push(container(control).width(Length::Fill));
        }
        cells = cells
            .push(row_icon_button(
                bootstrap_icons::CHEVRON_UP,
                edit_msg(target, &spec.key, ParamValueEdit::TableInsertRow(r)),
            ))
            .push(row_icon_button(
                bootstrap_icons::TRASH_3,
                edit_msg(target, &spec.key, ParamValueEdit::TableRemoveRow(r)),
            ));
        col = col.push(cells);
    }
    col = col.push(add_button(
        "Add row",
        edit_msg(target, &spec.key, ParamValueEdit::TableAddRow),
    ));
    container(col).padding(12.0).width(Length::Fill).style(common::banner_style).into()
}

/// The reserved width of a row's two action buttons (insert + remove) plus their spacing, so table
/// headers can align over their columns.
const ROW_ACTIONS_WIDTH: f32 = 72.0;

/// A small square icon button used for a row's insert/remove actions.
fn row_icon_button<'a>(glyph: &str, msg: Message) -> Elem<'a> {
    button(text(glyph.to_string()).font(fonts::BOOTSTRAP_ICONS).size(13.0))
        .style(button_style::secondary)
        .on_press(msg)
        .padding(Padding { top: 6.0, bottom: 6.0, left: 8.0, right: 8.0 })
        .into()
}

/// A small secondary "＋ Add …" button (matches the manifest editor's add buttons).
fn add_button<'a>(label: &str, msg: Message) -> Elem<'a> {
    button(
        row![
            text(bootstrap_icons::PLUS_LG).font(fonts::BOOTSTRAP_ICONS).size(11.0),
            text(label.to_string()).size(12.0),
        ]
        .spacing(6.0)
        .align_y(Vertical::Center),
    )
    .style(button_style::secondary)
    .on_press(msg)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smudgy_core::models::shared_packages::ParamOption;

    fn scalar(key: &str, kind: ParamKind) -> PackageParameter {
        PackageParameter {
            key: key.to_string(),
            label: None,
            secret: false,
            required: false,
            kind,
            default: None,
            options: Vec::new(),
            fields: Vec::new(),
        }
    }

    #[test]
    fn bool_seeds_and_projects() {
        let spec = scalar("flag", ParamKind::Bool);
        // Unset -> false; stored -> stored.
        assert!(matches!(seed(&spec, None), ParamValueState::Bool(false)));
        let state = seed(&spec, Some(&json!(true)));
        assert!(matches!(state, ParamValueState::Bool(true)));
        assert_eq!(to_json(&spec, &state).unwrap(), Some(json!(true)));
    }

    #[test]
    fn number_parses_on_project() {
        let spec = scalar("n", ParamKind::Number);
        let mut state = seed(&spec, Some(&json!(3)));
        assert_eq!(to_json(&spec, &state).unwrap(), Some(json!(3)));
        // A half-typed buffer is allowed in-state but errors on project.
        apply(&spec, &mut state, ParamValueEdit::Scalar(ScalarEdit::Text("x".to_string())));
        assert!(to_json(&spec, &state).is_err());
        // Cleared -> unset.
        apply(&spec, &mut state, ParamValueEdit::Scalar(ScalarEdit::Text("  ".to_string())));
        assert_eq!(to_json(&spec, &state).unwrap(), None);
    }

    #[test]
    fn dropdown_validates_against_options() {
        let mut spec = scalar("mode", ParamKind::Dropdown);
        spec.options = vec![
            ParamOption { value: "a".to_string(), label: None },
            ParamOption { value: "b".to_string(), label: Some("Bee".to_string()) },
        ];
        // A stored value that is no longer an option seeds as unset.
        assert!(matches!(seed(&spec, Some(&json!("gone"))), ParamValueState::Choice(None)));
        let state = seed(&spec, Some(&json!("b")));
        assert_eq!(to_json(&spec, &state).unwrap(), Some(json!("b")));
        // A forged out-of-set choice is rejected on project.
        let bad = ParamValueState::Choice(Some("z".to_string()));
        assert!(to_json(&spec, &bad).is_err());
    }

    #[test]
    fn list_round_trips_and_prunes_blanks() {
        let mut spec = scalar("aliases", ParamKind::List);
        spec.fields = vec![scalar("item", ParamKind::String)];
        let mut state = seed(&spec, Some(&json!(["north", "south"])));
        assert!(matches!(&state, ParamValueState::List(v) if v.len() == 2));
        // Insert above index 1, leave it blank -> pruned on project.
        apply(&spec, &mut state, ParamValueEdit::ListInsert(1));
        assert_eq!(to_json(&spec, &state).unwrap(), Some(json!(["north", "south"])));
        // Remove all -> unset.
        apply(&spec, &mut state, ParamValueEdit::ListRemove(0));
        apply(&spec, &mut state, ParamValueEdit::ListRemove(0));
        apply(&spec, &mut state, ParamValueEdit::ListRemove(0));
        assert_eq!(to_json(&spec, &state).unwrap(), None);
    }

    #[test]
    fn table_round_trips_with_typed_columns() {
        let mut spec = scalar("routes", ParamKind::Table);
        spec.fields = vec![scalar("from", ParamKind::String), scalar("hops", ParamKind::Number)];
        let stored = json!([{ "from": "inn", "hops": 3 }]);
        let mut state = seed(&spec, Some(&stored));
        assert_eq!(to_json(&spec, &state).unwrap(), Some(stored));
        // A fully-blank appended row is dropped on project.
        apply(&spec, &mut state, ParamValueEdit::TableAddRow);
        assert_eq!(
            to_json(&spec, &state).unwrap(),
            Some(json!([{ "from": "inn", "hops": 3 }]))
        );
        // Editing a cell flows back through.
        apply(
            &spec,
            &mut state,
            ParamValueEdit::TableSet(0, 0, ScalarEdit::Text("guild".to_string())),
        );
        assert_eq!(
            to_json(&spec, &state).unwrap(),
            Some(json!([{ "from": "guild", "hops": 3 }]))
        );
    }

    #[test]
    fn table_partial_row_omits_blank_cells_not_null() {
        // A row with one filled + one blank cell stores only the filled key — never a JSON `null`,
        // so a stored row stays within the `Record<string, ParamScalar>` shape the typings promise.
        let mut spec = scalar("routes", ParamKind::Table);
        spec.fields = vec![scalar("from", ParamKind::String), scalar("hops", ParamKind::Number)];
        let mut state = seed(&spec, None);
        apply(&spec, &mut state, ParamValueEdit::TableAddRow);
        apply(&spec, &mut state, ParamValueEdit::TableSet(0, 0, ScalarEdit::Text("inn".to_string())));
        let stored = to_json(&spec, &state).unwrap().expect("a non-blank row is stored");
        assert_eq!(stored, json!([{ "from": "inn" }]));
        // The blank cell is an absent key, not null, and round-trips identically.
        let row = &stored.as_array().unwrap()[0];
        assert!(row.get("hops").is_none());
        assert_eq!(to_json(&spec, &seed(&spec, Some(&stored))).unwrap(), Some(stored));
    }
}
