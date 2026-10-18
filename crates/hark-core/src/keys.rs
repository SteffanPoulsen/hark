//! The window's rebindable keys: the actions, each one's default chord, and the chords the
//! config overrides. A chord is `Ctrl+Shift+Alt+Key` text; the key name is the window's
//! (`E`, `Space`, `ArrowLeft`, `OpenBracket`), which the window resolves and the CLI only
//! carries. Navigation is not here, being positional, and neither is copy,
//! which stays the desktop's own Ctrl+C.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Play,
    PlayFromStart,
    Favorite,
    Loop,
    Autoplay,
    Preview,
    SelectionStart,
    SelectionEnd,
    PreviousTake,
    NextTake,
    Trim,
    Reverse,
    Zoom,
    ClearSelection,
    Export,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
}

impl Action {
    pub const ALL: [Action; 19] = [
        Action::Play,
        Action::PlayFromStart,
        Action::Favorite,
        Action::Loop,
        Action::Autoplay,
        Action::Preview,
        Action::SelectionStart,
        Action::SelectionEnd,
        Action::PreviousTake,
        Action::NextTake,
        Action::Trim,
        Action::Reverse,
        Action::Zoom,
        Action::ClearSelection,
        Action::Export,
        Action::NewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PreviousTab,
    ];

    /// The name in the config file and on the command line.
    pub fn id(self) -> &'static str {
        match self {
            Action::Play => "play",
            Action::PlayFromStart => "play_from_start",
            Action::Favorite => "favorite",
            Action::Loop => "loop",
            Action::Autoplay => "autoplay",
            Action::Preview => "preview",
            Action::SelectionStart => "selection_start",
            Action::SelectionEnd => "selection_end",
            Action::PreviousTake => "previous_take",
            Action::NextTake => "next_take",
            Action::Trim => "trim",
            Action::Reverse => "reverse",
            Action::Zoom => "zoom",
            Action::ClearSelection => "clear_selection",
            Action::Export => "export",
            Action::NewTab => "new_tab",
            Action::CloseTab => "close_tab",
            Action::NextTab => "next_tab",
            Action::PreviousTab => "previous_tab",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Play => "Play",
            Action::PlayFromStart => "Play from the start",
            Action::Favorite => "Favorite",
            Action::Loop => "Loop",
            Action::Autoplay => "Autoplay",
            Action::Preview => "Preview export",
            Action::SelectionStart => "Selection start",
            Action::SelectionEnd => "Selection end",
            Action::PreviousTake => "Previous take",
            Action::NextTake => "Next take",
            Action::Trim => "Trim",
            Action::Reverse => "Reverse",
            Action::Zoom => "Zoom to selection",
            Action::ClearSelection => "Clear selection",
            Action::Export => "Export",
            Action::NewTab => "New tab",
            Action::CloseTab => "Close tab",
            Action::NextTab => "Next tab",
            Action::PreviousTab => "Previous tab",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Action::Play => "Play the selection or the whole file; stop when playing. With Shift, play again.",
            Action::PlayFromStart => "Play the file from its start.",
            Action::Favorite => "Toggle the file's favorite mark.",
            Action::Loop => "Toggle looping.",
            Action::Autoplay => "Toggle playing a file when it is selected.",
            Action::Preview => "Toggle hearing and seeing the file as the export card would write it: its channels and its level.",
            Action::SelectionStart => "Set the selection start at the playhead.",
            Action::SelectionEnd => "Set the selection end at the playhead.",
            Action::PreviousTake => "Select the previous take, the spans of sound parted by silence.",
            Action::NextTake => "Select the next take.",
            Action::Trim => "Trim the selection, or the whole file, to the sound.",
            Action::Reverse => "Turn the file around, heard, drawn and exported backwards; the selection stays on the same sound.",
            Action::Zoom => "Fit the selection to the waveform.",
            Action::ClearSelection => "Clear the selection and its fades.",
            Action::Export => "Export the selection with the chosen preset.",
            Action::NewTab => "Open a tab on the list as it is now.",
            Action::CloseTab => "Close the list's tab; the last one stays.",
            Action::NextTab => "Show the list's next tab.",
            Action::PreviousTab => "Show the list's previous tab.",
        }
    }

    pub fn default_chord(self) -> &'static str {
        match self {
            Action::Play => "Space",
            Action::PlayFromStart => "Enter",
            Action::Favorite => "F",
            Action::Loop => "L",
            Action::Autoplay => "A",
            Action::Preview => "P",
            Action::SelectionStart => "OpenBracket",
            Action::SelectionEnd => "CloseBracket",
            Action::PreviousTake => "ArrowLeft",
            Action::NextTake => "ArrowRight",
            Action::Trim => "T",
            Action::Reverse => "R",
            Action::Zoom => "Z",
            Action::ClearSelection => "Escape",
            Action::Export => "E",
            Action::NewTab => "Ctrl+T",
            Action::CloseTab => "Ctrl+W",
            Action::NextTab => "Ctrl+Tab",
            Action::PreviousTab => "Ctrl+Shift+Tab",
        }
    }
}

/// A chord taken apart: the modifiers and the key name as the window spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: String,
}

impl Chord {
    /// Modifiers in any order and case, `+`-joined, the key last.
    pub fn parse(text: &str) -> Result<Chord, String> {
        let mut chord = Chord { ctrl: false, shift: false, alt: false, key: String::new() };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, mods)) = parts.split_last() else { return Err("empty chord".into()) };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" | "option" => chord.alt = true,
                other => return Err(format!("unknown modifier {other}")),
            }
        }
        if key.is_empty() {
            return Err("no key in the chord".into());
        }
        chord.key = if key.len() == 1 { key.to_ascii_uppercase() } else { key.to_string() };
        Ok(chord)
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (on, name) in [(self.ctrl, "Ctrl"), (self.shift, "Shift"), (self.alt, "Alt")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key)
    }
}

/// The chords that differ from the defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Keymap(BTreeMap<Action, String>);

/// A binding of an action hark no longer has is dropped, so a config written by an older
/// version still loads.
impl<'de> Deserialize<'de> for Keymap {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Keymap, D::Error> {
        let raw = BTreeMap::<String, String>::deserialize(d)?;
        Ok(Keymap(raw.into_iter().filter_map(|(id, chord)| Some((Action::from_id(&id)?, chord))).collect()))
    }
}

impl Keymap {
    pub fn chord(&self, action: Action) -> &str {
        self.0.get(&action).map(String::as_str).unwrap_or(action.default_chord())
    }

    /// Binds the chord, in its canonical spelling; the default binding is stored as none.
    pub fn set(&mut self, action: Action, chord: &str) -> Result<(), String> {
        let chord = Chord::parse(chord)?.to_string();
        if chord == action.default_chord() {
            self.0.remove(&action);
        } else {
            self.0.insert(action, chord);
        }
        Ok(())
    }

    pub fn reset(&mut self, action: Action) {
        self.0.remove(&action);
    }

    pub fn reset_all(&mut self) {
        self.0.clear();
    }

    pub fn is_default(&self, action: Action) -> bool {
        !self.0.contains_key(&action)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The other actions bound to the same chord.
    pub fn conflicts(&self, action: Action) -> Vec<Action> {
        let chord = self.chord(action);
        Action::ALL.into_iter().filter(|&a| a != action && self.chord(a) == chord).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config that binds an action since retired still loads, keeping its other bindings.
    #[test]
    fn retired_actions_are_dropped() {
        let keys: Keymap = serde_json::from_str(r#"{"stop": "S", "export": "Ctrl+E"}"#).unwrap();
        assert_eq!(keys.chord(Action::Export), "Ctrl+E");
        assert_eq!(serde_json::to_string(&keys).unwrap(), r#"{"export":"Ctrl+E"}"#);
    }
}
