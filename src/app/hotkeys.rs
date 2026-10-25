//! Rebindable hotkeys (Maya's Hotkey Editor) and the window to change them.
//! Bindings are saved with eframe's app storage, so they survive restarts.
//! File-level shortcuts (⌘S, ⌘O, ⌘N, ⌘Z) stay fixed, like OS conventions.

use std::collections::HashMap;
use std::fmt;

use egui::{Key, Modifiers, RichText, Ui};
use serde::{Deserialize, Serialize};

use super::theme;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum Action {
    SelectTool,
    MoveTool,
    RotateTool,
    ScaleTool,
    Wireframe,
    Shaded,
    FrameSelected,
    FrameAll,
    Delete,
    Duplicate,
    Group,
    Parent,
    Unparent,
    HideSelection,
    ShowSelection,
    ToggleFourView,
    PlayToggle,
    /// Held: snap the manipulator to the grid.
    GridSnapHold,
    /// Held: the manipulator moves the pivot instead of the object.
    PivotHold,
    /// With a brush tool: the lead clay's next finer / coarser detail
    /// level (`detail_level`). With a brush tool these win over a hold
    /// on the same key (D is also Edit Pivot, which brushes do not use).
    LevelFiner,
    LevelCoarser,
    /// With a brush tool: X symmetry on or off (wins over Snap to Grid's X,
    /// which brushes do not use).
    SymmetryX,
}

impl Action {
    pub const ALL: [Action; 22] = [
        Action::SelectTool,
        Action::MoveTool,
        Action::RotateTool,
        Action::ScaleTool,
        Action::Wireframe,
        Action::Shaded,
        Action::FrameSelected,
        Action::FrameAll,
        Action::Delete,
        Action::Duplicate,
        Action::Group,
        Action::Parent,
        Action::Unparent,
        Action::HideSelection,
        Action::ShowSelection,
        Action::ToggleFourView,
        Action::PlayToggle,
        Action::GridSnapHold,
        Action::PivotHold,
        Action::LevelFiner,
        Action::LevelCoarser,
        Action::SymmetryX,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::SelectTool => "Select Tool",
            Action::MoveTool => "Move Tool",
            Action::RotateTool => "Rotate Tool",
            Action::ScaleTool => "Scale Tool",
            Action::Wireframe => "Wireframe",
            Action::Shaded => "Smooth Shade All",
            Action::FrameSelected => "Frame Selection",
            Action::FrameAll => "Frame All",
            Action::Delete => "Delete",
            Action::Duplicate => "Duplicate",
            Action::Group => "Group",
            Action::Parent => "Parent",
            Action::Unparent => "Unparent",
            Action::HideSelection => "Hide Selection",
            Action::ShowSelection => "Show Selection",
            Action::ToggleFourView => "Toggle Single / Four View",
            Action::PlayToggle => "Play / Stop",
            Action::GridSnapHold => "Snap to Grid (hold)",
            Action::PivotHold => "Edit Pivot (hold)",
            Action::LevelFiner => "Finer Detail Level (brush tools)",
            Action::LevelCoarser => "Coarser Detail Level (brush tools)",
            Action::SymmetryX => "X Symmetry (brush tools)",
        }
    }

    pub fn is_hold(self) -> bool {
        matches!(self, Action::GridSnapHold | Action::PivotHold)
    }
}

/// A key plus modifiers. `command` is ⌘ on macOS and Ctrl elsewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Binding {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub command: bool,
}

impl Binding {
    const fn plain(key: Key) -> Self {
        Self { key, ctrl: false, shift: false, alt: false, command: false }
    }

    fn from_event(key: Key, m: Modifiers) -> Self {
        if cfg!(target_os = "macos") {
            Self { key, ctrl: m.ctrl, shift: m.shift, alt: m.alt, command: m.mac_cmd }
        } else {
            Self { key, ctrl: m.ctrl || m.command, shift: m.shift, alt: m.alt, command: false }
        }
    }

    pub fn matches(&self, key: Key, m: Modifiers) -> bool {
        let pressed = Self::from_event(key, m);
        let mut want = *self;
        if !cfg!(target_os = "macos") {
            want.ctrl |= want.command;
            want.command = false;
        }
        pressed == want
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
        for (on, name) in [(self.ctrl, "Ctrl"), (self.command, cmd), (self.alt, "Alt"), (self.shift, "Shift")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key.name())
    }
}

impl From<Binding> for String {
    fn from(b: Binding) -> String {
        // Stored form always says "Cmd" for the command modifier.
        let mut s = String::new();
        for (on, name) in [(b.ctrl, "Ctrl"), (b.command, "Cmd"), (b.alt, "Alt"), (b.shift, "Shift")] {
            if on {
                s.push_str(name);
                s.push('+');
            }
        }
        s + b.key.name()
    }
}

impl TryFrom<String> for Binding {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let mut parts: Vec<&str> = s.split('+').collect();
        // "Shift++" style keys are not supported; the key is the last part.
        let key_name = parts.pop().ok_or("empty binding")?;
        let key = Key::from_name(key_name).ok_or_else(|| format!("unknown key {key_name}"))?;
        let mut b = Binding::plain(key);
        for p in parts {
            match p {
                "Ctrl" => b.ctrl = true,
                "Cmd" => b.command = true,
                "Alt" => b.alt = true,
                "Shift" => b.shift = true,
                other => return Err(format!("unknown modifier {other}")),
            }
        }
        Ok(b)
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Hotkeys(pub HashMap<Action, Binding>);

impl Default for Hotkeys {
    fn default() -> Self {
        let ctrl = |key| Binding { ctrl: true, ..Binding::plain(key) };
        let shift = |key| Binding { shift: true, ..Binding::plain(key) };
        let cmd = |key| Binding { command: true, ..Binding::plain(key) };
        let alt = |key| Binding { alt: true, ..Binding::plain(key) };
        Hotkeys(HashMap::from([
            (Action::SelectTool, Binding::plain(Key::Q)),
            (Action::MoveTool, Binding::plain(Key::W)),
            (Action::RotateTool, Binding::plain(Key::E)),
            (Action::ScaleTool, Binding::plain(Key::R)),
            (Action::Wireframe, Binding::plain(Key::Num4)),
            (Action::Shaded, Binding::plain(Key::Num5)),
            (Action::FrameSelected, Binding::plain(Key::F)),
            (Action::FrameAll, Binding::plain(Key::A)),
            (Action::Delete, Binding::plain(Key::Backspace)),
            (Action::Duplicate, cmd(Key::D)),
            (Action::Group, ctrl(Key::G)),
            (Action::Parent, Binding::plain(Key::P)),
            (Action::Unparent, shift(Key::P)),
            (Action::HideSelection, ctrl(Key::H)),
            (Action::ShowSelection, shift(Key::H)),
            (Action::ToggleFourView, Binding::plain(Key::Space)),
            (Action::PlayToggle, alt(Key::V)),
            (Action::GridSnapHold, Binding::plain(Key::X)),
            (Action::PivotHold, Binding::plain(Key::D)),
            (Action::LevelFiner, Binding::plain(Key::D)),
            (Action::LevelCoarser, shift(Key::D)),
            (Action::SymmetryX, Binding::plain(Key::X)),
        ]))
    }
}

impl Hotkeys {
    /// Saved bindings with the default keys of actions added since they
    /// were saved; `known` lists the actions they already covered, so an
    /// action the user cleared stays cleared.
    pub fn with_new_defaults(mut self, known: &[Action]) -> Hotkeys {
        for (a, b) in Hotkeys::default().0 {
            if !known.contains(&a) {
                self.0.entry(a).or_insert(b);
            }
        }
        self
    }

    pub fn get(&self, action: Action) -> Option<Binding> {
        self.0.get(&action).copied()
    }

    /// Text for menus, e.g. "Ctrl+G", or "" when unbound.
    pub fn hint(&self, action: Action) -> String {
        self.get(action).map(|b| b.to_string()).unwrap_or_default()
    }

    /// Press-type actions triggered this frame.
    pub fn pressed(&self, ctx: &egui::Context) -> Vec<Action> {
        ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
                .flat_map(|(key, m)| {
                    self.0.iter().filter(move |(a, b)| !a.is_hold() && b.matches(key, m)).map(|(a, _)| *a)
                })
                .collect()
        })
    }

    pub fn held(&self, ctx: &egui::Context, action: Action) -> bool {
        self.get(action).is_some_and(|b| ctx.input(|i| i.key_down(b.key)))
    }
}

/// State for the Hotkey Editor window.
#[derive(Default)]
pub struct HotkeyEditor {
    pub open: bool,
    listening: Option<Action>,
    note: String,
}

impl HotkeyEditor {
    pub fn show(&mut self, ctx: &egui::Context, hotkeys: &mut Hotkeys) {
        if !self.open {
            return;
        }
        if let Some(action) = self.listening {
            self.capture(ctx, hotkeys, action);
        }
        let mut open = self.open;
        egui::Window::new("Hotkey Editor").open(&mut open).resizable(false).default_width(380.0).show(ctx, |ui| {
            ui.label(RichText::new("Click a key to change it, then press the new key. Esc cancels.").color(theme::TEXT_DIM));
            ui.add_space(4.0);
            egui::Grid::new("hotkeys").num_columns(3).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                for action in Action::ALL {
                    ui.label(action.label());
                    let text = if self.listening == Some(action) {
                        "press a key…".to_string()
                    } else {
                        hotkeys.get(action).map_or("—".into(), |b| b.to_string())
                    };
                    if ui.add(egui::Button::new(text).min_size(egui::vec2(120.0, 0.0))).clicked() {
                        self.listening = Some(action);
                        self.note.clear();
                    }
                    if ui.small_button("Clear").clicked() {
                        hotkeys.0.remove(&action);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Restore Defaults").clicked() {
                    *hotkeys = Hotkeys::default();
                    self.note = "Defaults restored.".into();
                }
                ui.label(RichText::new(&self.note).color(theme::TEXT_DIM));
            });
        });
        self.open = open;
        if !open {
            self.listening = None;
        }
    }

    fn capture(&mut self, ctx: &egui::Context, hotkeys: &mut Hotkeys, action: Action) {
        let pressed = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        let Some((key, m)) = pressed else { return };
        self.listening = None;
        if key == Key::Escape {
            return;
        }
        let binding = Binding::from_event(key, m);
        // One action per binding: steal it from whoever had it.
        let stolen: Vec<Action> = hotkeys.0.iter().filter(|(a, b)| **a != action && **b == binding).map(|(a, _)| *a).collect();
        for a in &stolen {
            hotkeys.0.remove(a);
        }
        hotkeys.0.insert(action, binding);
        self.note = match stolen.first() {
            Some(a) => format!("{binding} moved from \"{}\".", a.label()),
            None => format!("{} is now {binding}.", action.label()),
        };
        // Do not let the same key press also run the action.
        ctx.input_mut(|i| i.events.clear());
    }
}

pub fn menu_hint(ui: &mut Ui, label: &str, hint: String, enabled: bool) -> bool {
    let clicked = ui
        .add_enabled(enabled, egui::Button::new(label).shortcut_text(hint).min_size(egui::vec2(180.0, 0.0)))
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_as_text() {
        for (_, b) in Hotkeys::default().0 {
            let s: String = b.into();
            assert_eq!(Binding::try_from(s).unwrap(), b);
        }
        assert!(Binding::try_from("Hyper+Q".to_string()).is_err());
    }

    #[test]
    fn matching_needs_exact_modifiers() {
        let g = Binding { ctrl: true, ..Binding::plain(Key::G) };
        let ctrl = Modifiers { ctrl: true, ..Modifiers::NONE };
        assert!(g.matches(Key::G, ctrl));
        assert!(!g.matches(Key::G, Modifiers::NONE));
        assert!(!Binding::plain(Key::P).matches(Key::P, Modifiers::SHIFT));
    }

    /// Bindings saved before an action existed get its default key; one
    /// the user cleared (known when saved) stays cleared.
    #[test]
    fn new_actions_get_their_default_keys() {
        let mut saved = Hotkeys::default();
        saved.0.remove(&Action::LevelFiner);
        saved.0.remove(&Action::LevelCoarser);
        saved.0.remove(&Action::Group);
        let old: Vec<Action> = Action::ALL.into_iter().filter(|a| !matches!(a, Action::LevelFiner | Action::LevelCoarser | Action::SymmetryX)).collect();
        let h = saved.with_new_defaults(&old);
        assert_eq!(h.get(Action::LevelFiner), Some(Binding::plain(Key::D)));
        assert_eq!(h.get(Action::LevelCoarser), Some(Binding { shift: true, ..Binding::plain(Key::D) }));
        assert_eq!(h.get(Action::Group), None);
        let mut cleared = h.clone();
        cleared.0.remove(&Action::LevelFiner);
        assert_eq!(cleared.with_new_defaults(&Action::ALL).get(Action::LevelFiner), None);
    }

    #[test]
    fn hotkeys_serialize() {
        let h = Hotkeys::default();
        let back: Hotkeys = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(h, back);
    }
}
