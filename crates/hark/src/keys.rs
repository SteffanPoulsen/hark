//! The config's chords resolved against egui's keys: whether one was pressed this frame, and
//! how it reads in a hint.

use egui::{Context, Key, Modifiers};
use hark_core::keys::{Action, Chord, Keymap};

pub struct Binding {
    pub modifiers: Modifiers,
    pub key: Key,
}

/// `None` when the chord's key is not a name egui knows.
pub fn binding(chord: &str) -> Option<Binding> {
    let c = Chord::parse(chord).ok()?;
    let key = Key::from_name(&c.key)?;
    Some(Binding { modifiers: Modifiers { alt: c.alt, ctrl: false, shift: c.shift, mac_cmd: false, command: c.ctrl }, key })
}

/// The chord in a few characters: `Ctrl+C`, `←`, `Space`.
pub fn hint(chord: &str) -> String {
    let Some(b) = binding(chord) else { return chord.to_string() };
    let key = match b.key {
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        k => k.symbol_or_name(),
    };
    format!("{}{key}", prefix(b.modifiers))
}

fn prefix(m: Modifiers) -> String {
    let mut s = String::new();
    for (on, name) in [(m.command, "Ctrl+"), (m.shift, "Shift+"), (m.alt, "Alt+")] {
        if on {
            s.push_str(name);
        }
    }
    s
}

/// The chord an egui key event spells, for storing; `None` for a modifier key on its own,
/// which egui reports as a key of its own.
pub fn chord_of(key: Key, m: Modifiers) -> Option<String> {
    use Key::{AltLeft, AltRight, ControlLeft, ControlRight, ShiftLeft, ShiftRight, SuperLeft, SuperRight};
    if matches!(key, ShiftLeft | ShiftRight | ControlLeft | ControlRight | AltLeft | AltRight | SuperLeft | SuperRight) {
        return None;
    }
    Some(format!("{}{}", prefix(m), key.name()))
}

fn held(m: Modifiers, pattern: Modifiers) -> bool {
    m.alt == pattern.alt && m.shift == pattern.shift && m.command == pattern.command
}

/// Whether the action's chord went down this frame. Play also fires with Shift on top of its
/// chord; the window reads Shift to tell the two apart.
pub fn pressed(ctx: &Context, keys: &Keymap, action: Action) -> bool {
    let Some(b) = binding(keys.chord(action)) else { return false };
    ctx.input(|i| {
        let mut pattern = b.modifiers;
        if action == Action::Play && i.modifiers.shift {
            pattern.shift = true;
        }
        i.key_pressed(b.key) && held(i.modifiers, pattern)
    })
}
