//! The config's chords resolved against egui's keys: whether one was pressed this frame, and
//! how it reads in a hint.

use egui::{Context, Key, Modifiers};
use kuulo_core::keys::{Action, Chord, Keymap};

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

/// The command modifier as the platform names it; a stored chord says `Ctrl` everywhere.
pub const COMMAND: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

/// The modifier that makes a drag a copy on this platform, which starts a drag out of the
/// wave: Option on macOS, where Cmd during a drag asks for a move, and Ctrl elsewhere.
pub const DRAG_COPY: &str = if cfg!(target_os = "macos") { "Option" } else { "Ctrl" };

pub fn drag_copy_held(m: Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.alt
    } else {
        m.ctrl
    }
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
    format!("{}{key}", prefix(b.modifiers, COMMAND))
}

fn prefix(m: Modifiers, command: &str) -> String {
    let mut s = String::new();
    for (on, name) in [(m.command, command), (m.shift, "Shift"), (m.alt, "Alt")] {
        if on {
            s.push_str(name);
            s.push('+');
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
    Some(format!("{}{}", prefix(m, "Ctrl"), key.name()))
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
