use std::borrow::Cow;

/// The mappings defined in this file where created from reading the alacritty source
use gpui::Keystroke;

use crate::Modes;

#[derive(Debug, PartialEq, Eq)]
enum TerminalModifiers {
    None,
    Alt,
    Ctrl,
    Shift,
    CtrlShift,
    Other,
}

impl TerminalModifiers {
    fn new(ks: &Keystroke) -> Self {
        match (
            ks.modifiers.alt,
            ks.modifiers.control,
            ks.modifiers.shift,
            ks.modifiers.platform,
        ) {
            (false, false, false, false) => TerminalModifiers::None,
            (true, false, false, false) => TerminalModifiers::Alt,
            (false, true, false, false) => TerminalModifiers::Ctrl,
            (false, false, true, false) => TerminalModifiers::Shift,
            (false, true, true, false) => TerminalModifiers::CtrlShift,
            _ => TerminalModifiers::Other,
        }
    }

    fn any(&self) -> bool {
        match &self {
            TerminalModifiers::None => false,
            TerminalModifiers::Alt => true,
            TerminalModifiers::Ctrl => true,
            TerminalModifiers::Shift => true,
            TerminalModifiers::CtrlShift => true,
            TerminalModifiers::Other => true,
        }
    }
}

pub(crate) fn to_esc_str(
    keystroke: &Keystroke,
    mode: Modes,
    option_as_meta: bool,
) -> Option<Cow<'static, str>> {
    let modifiers = TerminalModifiers::new(keystroke);

    // Manual Bindings including modifiers
    let manual_esc_str: Option<&'static str> = match (keystroke.key.as_ref(), &modifiers) {
        //Basic special keys
        ("tab", TerminalModifiers::None) => Some("\x09"),
        ("escape", TerminalModifiers::None) => Some("\x1b"),
        ("enter", TerminalModifiers::None) => Some("\x0d"),
        ("enter", TerminalModifiers::Shift) => Some("\x0a"),
        ("enter", TerminalModifiers::Alt) => Some("\x1b\x0d"),
        ("backspace", TerminalModifiers::None) => Some("\x7f"),
        //Interesting escape codes
        ("tab", TerminalModifiers::Shift) => Some("\x1b[Z"),
        ("backspace", TerminalModifiers::Ctrl) => Some("\x08"),
        ("backspace", TerminalModifiers::Alt) => Some("\x1b\x7f"),
        ("backspace", TerminalModifiers::Shift) => Some("\x7f"),
        ("space", TerminalModifiers::Ctrl) => Some("\x00"),
        ("home", TerminalModifiers::None) if mode.contains(Modes::APP_CURSOR) => Some("\x1bOH"),
        ("home", TerminalModifiers::None) if !mode.contains(Modes::APP_CURSOR) => Some("\x1b[H"),
        ("end", TerminalModifiers::None) if mode.contains(Modes::APP_CURSOR) => Some("\x1bOF"),
        ("end", TerminalModifiers::None) if !mode.contains(Modes::APP_CURSOR) => Some("\x1b[F"),
        ("up", TerminalModifiers::None) if mode.contains(Modes::APP_CURSOR) => Some("\x1bOA"),
        ("up", TerminalModifiers::None) if !mode.contains(Modes::APP_CURSOR) => Some("\x1b[A"),
        ("down", TerminalModifiers::None) if mode.contains(Modes::APP_CURSOR) => Some("\x1bOB"),
        ("down", TerminalModifiers::None) if !mode.contains(Modes::APP_CURSOR) => Some("\x1b[B"),
        ("right", TerminalModifiers::None) if mode.contains(Modes::APP_CURSOR) => Some("\x1bOC"),
        ("right", TerminalModifiers::None) if !mode.contains(Modes::APP_CURSOR) => Some("\x1b[C"),
        ("left", TerminalModifiers::None) if mode.contains(Modes::APP_CURSOR) => Some("\x1bOD"),
        ("left", TerminalModifiers::None) if !mode.contains(Modes::APP_CURSOR) => Some("\x1b[D"),
        ("back", TerminalModifiers::None) => Some("\x7f"),
        ("insert", TerminalModifiers::None) => Some("\x1b[2~"),
        ("delete", TerminalModifiers::None) => Some("\x1b[3~"),
        ("pageup", TerminalModifiers::None) => Some("\x1b[5~"),
        ("pagedown", TerminalModifiers::None) => Some("\x1b[6~"),
        ("f1", TerminalModifiers::None) => Some("\x1bOP"),
        ("f2", TerminalModifiers::None) => Some("\x1bOQ"),
        ("f3", TerminalModifiers::None) => Some("\x1bOR"),
        ("f4", TerminalModifiers::None) => Some("\x1bOS"),
        ("f5", TerminalModifiers::None) => Some("\x1b[15~"),
        ("f6", TerminalModifiers::None) => Some("\x1b[17~"),
        ("f7", TerminalModifiers::None) => Some("\x1b[18~"),
        ("f8", TerminalModifiers::None) => Some("\x1b[19~"),
        ("f9", TerminalModifiers::None) => Some("\x1b[20~"),
        ("f10", TerminalModifiers::None) => Some("\x1b[21~"),
        ("f11", TerminalModifiers::None) => Some("\x1b[23~"),
        ("f12", TerminalModifiers::None) => Some("\x1b[24~"),
        ("f13", TerminalModifiers::None) => Some("\x1b[25~"),
        ("f14", TerminalModifiers::None) => Some("\x1b[26~"),
        ("f15", TerminalModifiers::None) => Some("\x1b[28~"),
        ("f16", TerminalModifiers::None) => Some("\x1b[29~"),
        ("f17", TerminalModifiers::None) => Some("\x1b[31~"),
        ("f18", TerminalModifiers::None) => Some("\x1b[32~"),
        ("f19", TerminalModifiers::None) => Some("\x1b[33~"),
        ("f20", TerminalModifiers::None) => Some("\x1b[34~"),
        // NumpadEnter, Action::Esc("\n".into());
        //Mappings for caret notation keys
        ("a", TerminalModifiers::Ctrl) => Some("\x01"), //1
        ("A", TerminalModifiers::CtrlShift) => Some("\x01"), //1
        ("b", TerminalModifiers::Ctrl) => Some("\x02"), //2
        ("B", TerminalModifiers::CtrlShift) => Some("\x02"), //2
        ("c", TerminalModifiers::Ctrl) => Some("\x03"), //3
        ("C", TerminalModifiers::CtrlShift) => Some("\x03"), //3
        ("d", TerminalModifiers::Ctrl) => Some("\x04"), //4
        ("D", TerminalModifiers::CtrlShift) => Some("\x04"), //4
        ("e", TerminalModifiers::Ctrl) => Some("\x05"), //5
        ("E", TerminalModifiers::CtrlShift) => Some("\x05"), //5
        ("f", TerminalModifiers::Ctrl) => Some("\x06"), //6
        ("F", TerminalModifiers::CtrlShift) => Some("\x06"), //6
        ("g", TerminalModifiers::Ctrl) => Some("\x07"), //7
        ("G", TerminalModifiers::CtrlShift) => Some("\x07"), //7
        ("h", TerminalModifiers::Ctrl) => Some("\x08"), //8
        ("H", TerminalModifiers::CtrlShift) => Some("\x08"), //8
        ("i", TerminalModifiers::Ctrl) => Some("\x09"), //9
        ("I", TerminalModifiers::CtrlShift) => Some("\x09"), //9
        ("j", TerminalModifiers::Ctrl) => Some("\x0a"), //10
        ("J", TerminalModifiers::CtrlShift) => Some("\x0a"), //10
        ("k", TerminalModifiers::Ctrl) => Some("\x0b"), //11
        ("K", TerminalModifiers::CtrlShift) => Some("\x0b"), //11
        ("l", TerminalModifiers::Ctrl) => Some("\x0c"), //12
        ("L", TerminalModifiers::CtrlShift) => Some("\x0c"), //12
        ("m", TerminalModifiers::Ctrl) => Some("\x0d"), //13
        ("M", TerminalModifiers::CtrlShift) => Some("\x0d"), //13
        ("n", TerminalModifiers::Ctrl) => Some("\x0e"), //14
        ("N", TerminalModifiers::CtrlShift) => Some("\x0e"), //14
        ("o", TerminalModifiers::Ctrl) => Some("\x0f"), //15
        ("O", TerminalModifiers::CtrlShift) => Some("\x0f"), //15
        ("p", TerminalModifiers::Ctrl) => Some("\x10"), //16
        ("P", TerminalModifiers::CtrlShift) => Some("\x10"), //16
        ("q", TerminalModifiers::Ctrl) => Some("\x11"), //17
        ("Q", TerminalModifiers::CtrlShift) => Some("\x11"), //17
        ("r", TerminalModifiers::Ctrl) => Some("\x12"), //18
        ("R", TerminalModifiers::CtrlShift) => Some("\x12"), //18
        ("s", TerminalModifiers::Ctrl) => Some("\x13"), //19
        ("S", TerminalModifiers::CtrlShift) => Some("\x13"), //19
        ("t", TerminalModifiers::Ctrl) => Some("\x14"), //20
        ("T", TerminalModifiers::CtrlShift) => Some("\x14"), //20
        ("u", TerminalModifiers::Ctrl) => Some("\x15"), //21
        ("U", TerminalModifiers::CtrlShift) => Some("\x15"), //21
        ("v", TerminalModifiers::Ctrl) => Some("\x16"), //22
        ("V", TerminalModifiers::CtrlShift) => Some("\x16"), //22
        ("w", TerminalModifiers::Ctrl) => Some("\x17"), //23
        ("W", TerminalModifiers::CtrlShift) => Some("\x17"), //23
        ("x", TerminalModifiers::Ctrl) => Some("\x18"), //24
        ("X", TerminalModifiers::CtrlShift) => Some("\x18"), //24
        ("y", TerminalModifiers::Ctrl) => Some("\x19"), //25
        ("Y", TerminalModifiers::CtrlShift) => Some("\x19"), //25
        ("z", TerminalModifiers::Ctrl) => Some("\x1a"), //26
        ("Z", TerminalModifiers::CtrlShift) => Some("\x1a"), //26
        ("@", TerminalModifiers::Ctrl) => Some("\x00"), //0
        ("[", TerminalModifiers::Ctrl) => Some("\x1b"), //27
        ("\\", TerminalModifiers::Ctrl) => Some("\x1c"), //28
        ("]", TerminalModifiers::Ctrl) => Some("\x1d"), //29
        ("^", TerminalModifiers::Ctrl) => Some("\x1e"), //30
        ("_", TerminalModifiers::Ctrl) => Some("\x1f"), //31
        ("?", TerminalModifiers::Ctrl) => Some("\x7f"), //127
        _ => None,
    };
    if let Some(esc_str) = manual_esc_str {
        return Some(Cow::Borrowed(esc_str));
    }

    // Automated bindings applying modifiers
    if modifiers.any() {
        let modifier_code = modifier_code(keystroke);
        let modified_esc_str = match keystroke.key.as_ref() {
            "up" => Some(format!("\x1b[1;{}A", modifier_code)),
            "down" => Some(format!("\x1b[1;{}B", modifier_code)),
            "right" => Some(format!("\x1b[1;{}C", modifier_code)),
            "left" => Some(format!("\x1b[1;{}D", modifier_code)),
            "f1" => Some(format!("\x1b[1;{}P", modifier_code)),
            "f2" => Some(format!("\x1b[1;{}Q", modifier_code)),
            "f3" => Some(format!("\x1b[1;{}R", modifier_code)),
            "f4" => Some(format!("\x1b[1;{}S", modifier_code)),
            "F5" => Some(format!("\x1b[15;{}~", modifier_code)),
            "f6" => Some(format!("\x1b[17;{}~", modifier_code)),
            "f7" => Some(format!("\x1b[18;{}~", modifier_code)),
            "f8" => Some(format!("\x1b[19;{}~", modifier_code)),
            "f9" => Some(format!("\x1b[20;{}~", modifier_code)),
            "f10" => Some(format!("\x1b[21;{}~", modifier_code)),
            "f11" => Some(format!("\x1b[23;{}~", modifier_code)),
            "f12" => Some(format!("\x1b[24;{}~", modifier_code)),
            "f13" => Some(format!("\x1b[25;{}~", modifier_code)),
            "f14" => Some(format!("\x1b[26;{}~", modifier_code)),
            "f15" => Some(format!("\x1b[28;{}~", modifier_code)),
            "f16" => Some(format!("\x1b[29;{}~", modifier_code)),
            "f17" => Some(format!("\x1b[31;{}~", modifier_code)),
            "f18" => Some(format!("\x1b[32;{}~", modifier_code)),
            "f19" => Some(format!("\x1b[33;{}~", modifier_code)),
            "f20" => Some(format!("\x1b[34;{}~", modifier_code)),
            "insert" => Some(format!("\x1b[2;{}~", modifier_code)),
            "pageup" => Some(format!("\x1b[5;{}~", modifier_code)),
            "pagedown" => Some(format!("\x1b[6;{}~", modifier_code)),
            "end" => Some(format!("\x1b[1;{}F", modifier_code)),
            "home" => Some(format!("\x1b[1;{}H", modifier_code)),
            _ => None,
        };
        if let Some(esc_str) = modified_esc_str {
            return Some(Cow::Owned(esc_str));
        }
    }

    if !cfg!(target_os = "macos") || option_as_meta {
        let is_alt_lowercase_ascii =
            modifiers == TerminalModifiers::Alt && keystroke.key.is_ascii();
        let is_alt_uppercase_ascii =
            keystroke.modifiers.alt && keystroke.modifiers.shift && keystroke.key.is_ascii();
        if is_alt_lowercase_ascii || is_alt_uppercase_ascii {
            let key = if is_alt_uppercase_ascii {
                &keystroke.key.to_ascii_uppercase()
            } else {
                &keystroke.key
            };
            return Some(Cow::Owned(format!("\x1b{}", key)));
        }
    }

    None
}

#[cfg(target_os = "windows")]
pub(crate) fn to_win32_input_sequence(keystroke: &Keystroke) -> Option<String> {
    let key = Win32Key::from_keystroke(keystroke)?;
    let unicode_char = key.unicode_char(keystroke)?;
    let control_key_state = control_key_state(keystroke, key.enhanced);

    Some(format!(
        "\x1b[{};{};{};1;{};1_",
        key.virtual_key, key.scan_code, unicode_char, control_key_state
    ))
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
struct Win32Key {
    virtual_key: u16,
    scan_code: u16,
    enhanced: bool,
    unshifted: Option<char>,
    shifted: Option<char>,
}

#[cfg(target_os = "windows")]
impl Win32Key {
    fn from_keystroke(keystroke: &Keystroke) -> Option<Self> {
        match keystroke.key.as_str() {
            "backspace" | "back" => Some(Self::named(0x08, 0x0e, Some('\x08'))),
            "tab" => Some(Self::named(0x09, 0x0f, Some('\t'))),
            "enter" => Some(Self::named(0x0d, 0x1c, Some('\r'))),
            "escape" => Some(Self::named(0x1b, 0x01, Some('\x1b'))),
            "space" => Some(Self::named(0x20, 0x39, Some(' '))),
            "pageup" => Some(Self::enhanced(0x21, 0x49)),
            "pagedown" => Some(Self::enhanced(0x22, 0x51)),
            "end" => Some(Self::enhanced(0x23, 0x4f)),
            "home" => Some(Self::enhanced(0x24, 0x47)),
            "left" => Some(Self::enhanced(0x25, 0x4b)),
            "up" => Some(Self::enhanced(0x26, 0x48)),
            "right" => Some(Self::enhanced(0x27, 0x4d)),
            "down" => Some(Self::enhanced(0x28, 0x50)),
            "insert" => Some(Self::enhanced(0x2d, 0x52)),
            "delete" => Some(Self::enhanced(0x2e, 0x53)),
            "f1" => Some(Self::function_key(1, 0x3b)),
            "f2" => Some(Self::function_key(2, 0x3c)),
            "f3" => Some(Self::function_key(3, 0x3d)),
            "f4" => Some(Self::function_key(4, 0x3e)),
            "f5" => Some(Self::function_key(5, 0x3f)),
            "f6" => Some(Self::function_key(6, 0x40)),
            "f7" => Some(Self::function_key(7, 0x41)),
            "f8" => Some(Self::function_key(8, 0x42)),
            "f9" => Some(Self::function_key(9, 0x43)),
            "f10" => Some(Self::function_key(10, 0x44)),
            "f11" => Some(Self::function_key(11, 0x57)),
            "f12" => Some(Self::function_key(12, 0x58)),
            _ => keystroke
                .key
                .chars()
                .next()
                .filter(|_| keystroke.key.chars().count() == 1)
                .and_then(Self::from_char),
        }
    }

    fn named(virtual_key: u16, scan_code: u16, unshifted: Option<char>) -> Self {
        Self {
            virtual_key,
            scan_code,
            enhanced: false,
            unshifted,
            shifted: unshifted,
        }
    }

    fn enhanced(virtual_key: u16, scan_code: u16) -> Self {
        Self {
            virtual_key,
            scan_code,
            enhanced: true,
            unshifted: None,
            shifted: None,
        }
    }

    fn function_key(number: u16, scan_code: u16) -> Self {
        Self::named(0x70 + number - 1, scan_code, None)
    }

    fn printable(virtual_key: u16, scan_code: u16, unshifted: char, shifted: char) -> Self {
        Self {
            virtual_key,
            scan_code,
            enhanced: false,
            unshifted: Some(unshifted),
            shifted: Some(shifted),
        }
    }

    fn from_char(character: char) -> Option<Self> {
        let lower = character.to_ascii_lowercase();
        match lower {
            'a' => Some(Self::printable(0x41, 0x1e, 'a', 'A')),
            'b' => Some(Self::printable(0x42, 0x30, 'b', 'B')),
            'c' => Some(Self::printable(0x43, 0x2e, 'c', 'C')),
            'd' => Some(Self::printable(0x44, 0x20, 'd', 'D')),
            'e' => Some(Self::printable(0x45, 0x12, 'e', 'E')),
            'f' => Some(Self::printable(0x46, 0x21, 'f', 'F')),
            'g' => Some(Self::printable(0x47, 0x22, 'g', 'G')),
            'h' => Some(Self::printable(0x48, 0x23, 'h', 'H')),
            'i' => Some(Self::printable(0x49, 0x17, 'i', 'I')),
            'j' => Some(Self::printable(0x4a, 0x24, 'j', 'J')),
            'k' => Some(Self::printable(0x4b, 0x25, 'k', 'K')),
            'l' => Some(Self::printable(0x4c, 0x26, 'l', 'L')),
            'm' => Some(Self::printable(0x4d, 0x32, 'm', 'M')),
            'n' => Some(Self::printable(0x4e, 0x31, 'n', 'N')),
            'o' => Some(Self::printable(0x4f, 0x18, 'o', 'O')),
            'p' => Some(Self::printable(0x50, 0x19, 'p', 'P')),
            'q' => Some(Self::printable(0x51, 0x10, 'q', 'Q')),
            'r' => Some(Self::printable(0x52, 0x13, 'r', 'R')),
            's' => Some(Self::printable(0x53, 0x1f, 's', 'S')),
            't' => Some(Self::printable(0x54, 0x14, 't', 'T')),
            'u' => Some(Self::printable(0x55, 0x16, 'u', 'U')),
            'v' => Some(Self::printable(0x56, 0x2f, 'v', 'V')),
            'w' => Some(Self::printable(0x57, 0x11, 'w', 'W')),
            'x' => Some(Self::printable(0x58, 0x2d, 'x', 'X')),
            'y' => Some(Self::printable(0x59, 0x15, 'y', 'Y')),
            'z' => Some(Self::printable(0x5a, 0x2c, 'z', 'Z')),
            '0' | ')' => Some(Self::printable(0x30, 0x0b, '0', ')')),
            '1' | '!' => Some(Self::printable(0x31, 0x02, '1', '!')),
            '2' | '@' => Some(Self::printable(0x32, 0x03, '2', '@')),
            '3' | '#' => Some(Self::printable(0x33, 0x04, '3', '#')),
            '4' | '$' => Some(Self::printable(0x34, 0x05, '4', '$')),
            '5' | '%' => Some(Self::printable(0x35, 0x06, '5', '%')),
            '6' | '^' => Some(Self::printable(0x36, 0x07, '6', '^')),
            '7' | '&' => Some(Self::printable(0x37, 0x08, '7', '&')),
            '8' | '*' => Some(Self::printable(0x38, 0x09, '8', '*')),
            '9' | '(' => Some(Self::printable(0x39, 0x0a, '9', '(')),
            '-' | '_' => Some(Self::printable(0xbd, 0x0c, '-', '_')),
            '=' | '+' => Some(Self::printable(0xbb, 0x0d, '=', '+')),
            '[' | '{' => Some(Self::printable(0xdb, 0x1a, '[', '{')),
            ']' | '}' => Some(Self::printable(0xdd, 0x1b, ']', '}')),
            '\\' | '|' => Some(Self::printable(0xdc, 0x2b, '\\', '|')),
            ';' | ':' => Some(Self::printable(0xba, 0x27, ';', ':')),
            '\'' | '"' => Some(Self::printable(0xde, 0x28, '\'', '"')),
            '`' | '~' => Some(Self::printable(0xc0, 0x29, '`', '~')),
            ',' | '<' => Some(Self::printable(0xbc, 0x33, ',', '<')),
            '.' | '>' => Some(Self::printable(0xbe, 0x34, '.', '>')),
            '/' | '?' => Some(Self::printable(0xbf, 0x35, '/', '?')),
            _ => None,
        }
    }

    fn unicode_char(self, keystroke: &Keystroke) -> Option<u16> {
        if keystroke.modifiers.control {
            if let Some(control_char) = control_char(keystroke.key.as_str()) {
                return Some(control_char);
            }
        }

        if let Some(key_char) = keystroke
            .key_char
            .as_deref()
            .and_then(single_utf16_code_unit)
        {
            return Some(key_char);
        }

        let character = if keystroke.modifiers.shift {
            self.shifted
        } else {
            self.unshifted
        };

        character.map(|character| character as u16).or(Some(0))
    }
}

#[cfg(target_os = "windows")]
fn control_key_state(keystroke: &Keystroke, enhanced: bool) -> u16 {
    const LEFT_ALT_PRESSED: u16 = 0x0002;
    const LEFT_CTRL_PRESSED: u16 = 0x0008;
    const SHIFT_PRESSED: u16 = 0x0010;
    const ENHANCED_KEY: u16 = 0x0100;

    let mut state = 0;
    if keystroke.modifiers.alt {
        state |= LEFT_ALT_PRESSED;
    }
    if keystroke.modifiers.control {
        state |= LEFT_CTRL_PRESSED;
    }
    if keystroke.modifiers.shift {
        state |= SHIFT_PRESSED;
    }
    if enhanced {
        state |= ENHANCED_KEY;
    }
    state
}

#[cfg(target_os = "windows")]
fn control_char(key: &str) -> Option<u16> {
    match key {
        "@" | "space" => Some(0),
        "[" | "escape" => Some(27),
        "\\" => Some(28),
        "]" => Some(29),
        "^" => Some(30),
        "_" => Some(31),
        "?" | "delete" => Some(127),
        "backspace" | "back" => Some(8),
        "tab" => Some(9),
        "enter" => Some(13),
        key if key.len() == 1 => {
            let character = key.as_bytes()[0].to_ascii_lowercase();
            if character.is_ascii_lowercase() {
                Some((character - b'a' + 1) as u16)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn single_utf16_code_unit(text: &str) -> Option<u16> {
    let mut units = text.encode_utf16();
    let unit = units.next()?;
    if units.next().is_some() {
        None
    } else {
        Some(unit)
    }
}

///   Code     Modifiers
/// ---------+---------------------------
///    2     | Shift
///    3     | Alt
///    4     | Shift + Alt
///    5     | Control
///    6     | Shift + Control
///    7     | Alt + Control
///    8     | Shift + Alt + Control
/// ---------+---------------------------
/// from: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-PC-Style-Function-Keys
fn modifier_code(keystroke: &Keystroke) -> u32 {
    let mut modifier_code = 0;
    if keystroke.modifiers.shift {
        modifier_code |= 1;
    }
    if keystroke.modifiers.alt {
        modifier_code |= 1 << 1;
    }
    if keystroke.modifiers.control {
        modifier_code |= 1 << 2;
    }
    modifier_code + 1
}

#[cfg(test)]
mod test {
    use gpui::Modifiers;

    use super::*;

    #[test]
    fn test_plain_inputs() {
        let ks = Keystroke {
            modifiers: Modifiers {
                control: false,
                alt: false,
                shift: false,
                platform: false,
                function: false,
            },
            key: "🖖🏻".to_string(), //2 char string
            key_char: None,
        };
        assert_eq!(to_esc_str(&ks, Modes::NONE, false), None);
    }

    #[test]
    fn test_application_mode() {
        let app_cursor = Modes::APP_CURSOR;
        let none = Modes::NONE;

        let up = Keystroke::parse("up").unwrap();
        let down = Keystroke::parse("down").unwrap();
        let left = Keystroke::parse("left").unwrap();
        let right = Keystroke::parse("right").unwrap();

        assert_eq!(to_esc_str(&up, none, false), Some("\x1b[A".into()));
        assert_eq!(to_esc_str(&down, none, false), Some("\x1b[B".into()));
        assert_eq!(to_esc_str(&right, none, false), Some("\x1b[C".into()));
        assert_eq!(to_esc_str(&left, none, false), Some("\x1b[D".into()));

        assert_eq!(to_esc_str(&up, app_cursor, false), Some("\x1bOA".into()));
        assert_eq!(to_esc_str(&down, app_cursor, false), Some("\x1bOB".into()));
        assert_eq!(to_esc_str(&right, app_cursor, false), Some("\x1bOC".into()));
        assert_eq!(to_esc_str(&left, app_cursor, false), Some("\x1bOD".into()));

        let home = Keystroke::parse("home").unwrap();
        let end = Keystroke::parse("end").unwrap();
        assert_eq!(to_esc_str(&home, none, false), Some("\x1b[H".into()));
        assert_eq!(to_esc_str(&end, none, false), Some("\x1b[F".into()));
        assert_eq!(to_esc_str(&home, app_cursor, false), Some("\x1bOH".into()));
        assert_eq!(to_esc_str(&end, app_cursor, false), Some("\x1bOF".into()));

        let shift_up = Keystroke::parse("shift-up").unwrap();
        let shift_down = Keystroke::parse("shift-down").unwrap();
        let shift_home = Keystroke::parse("shift-home").unwrap();
        let shift_end = Keystroke::parse("shift-end").unwrap();
        assert_eq!(to_esc_str(&shift_up, none, false), Some("\x1b[1;2A".into()));
        assert_eq!(
            to_esc_str(&shift_down, none, false),
            Some("\x1b[1;2B".into())
        );
        assert_eq!(
            to_esc_str(&shift_home, none, false),
            Some("\x1b[1;2H".into())
        );
        assert_eq!(
            to_esc_str(&shift_end, none, false),
            Some("\x1b[1;2F".into())
        );
    }

    #[test]
    fn test_ctrl_codes() {
        let letters_lower = 'a'..='z';
        let letters_upper = 'A'..='Z';
        let mode = Modes::APP_CURSOR;

        for (lower, upper) in letters_lower.zip(letters_upper) {
            assert_eq!(
                to_esc_str(
                    &Keystroke::parse(&format!("ctrl-shift-{}", lower)).unwrap(),
                    mode,
                    false
                ),
                to_esc_str(
                    &Keystroke::parse(&format!("ctrl-{}", upper)).unwrap(),
                    mode,
                    false
                ),
                "On letter: {}/{}",
                lower,
                upper
            )
        }
    }

    #[test]
    fn test_ctrl_j_line_ending() {
        assert_eq!(
            to_esc_str(
                &Keystroke::parse("ctrl-j").unwrap(),
                Modes::APP_CURSOR,
                false
            ),
            Some("\x0a".into())
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_win32_input_ctrl_j() {
        assert_eq!(
            to_win32_input_sequence(&Keystroke::parse("ctrl-j").unwrap()),
            Some("\x1b[74;36;10;1;8;1_".into())
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_win32_input_shift_enter() {
        assert_eq!(
            to_win32_input_sequence(&Keystroke::parse("shift-enter").unwrap()),
            Some("\x1b[13;28;13;1;16;1_".into())
        );
    }

    #[test]
    fn alt_is_meta() {
        let ascii_printable = ' '..='~';
        for character in ascii_printable {
            assert_eq!(
                to_esc_str(
                    &Keystroke::parse(&format!("alt-{}", character)).unwrap(),
                    Modes::NONE,
                    true
                )
                .unwrap(),
                format!("\x1b{}", character)
            );
        }

        let gpui_keys = [
            "up", "down", "right", "left", "f1", "f2", "f3", "f4", "F5", "f6", "f7", "f8", "f9",
            "f10", "f11", "f12", "f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "insert",
            "pageup", "pagedown", "end", "home",
        ];

        for key in gpui_keys {
            assert_ne!(
                to_esc_str(
                    &Keystroke::parse(&format!("alt-{}", key)).unwrap(),
                    Modes::NONE,
                    true
                )
                .unwrap(),
                format!("\x1b{}", key)
            );
        }
    }

    #[test]
    fn test_shift_enter_newline() {
        let shift_enter = Keystroke::parse("shift-enter").unwrap();
        let regular_enter = Keystroke::parse("enter").unwrap();
        let mode = Modes::NONE;

        // Shift-enter should send line feed (newline)
        assert_eq!(to_esc_str(&shift_enter, mode, false), Some("\x0a".into()));

        // Regular enter should still send carriage return
        assert_eq!(to_esc_str(&regular_enter, mode, false), Some("\x0d".into()));
    }

    #[test]
    fn test_modifier_code_calc() {
        //   Code     Modifiers
        // ---------+---------------------------
        //    2     | Shift
        //    3     | Alt
        //    4     | Shift + Alt
        //    5     | Control
        //    6     | Shift + Control
        //    7     | Alt + Control
        //    8     | Shift + Alt + Control
        // ---------+---------------------------
        // from: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-PC-Style-Function-Keys
        assert_eq!(2, modifier_code(&Keystroke::parse("shift-a").unwrap()));
        assert_eq!(3, modifier_code(&Keystroke::parse("alt-a").unwrap()));
        assert_eq!(4, modifier_code(&Keystroke::parse("shift-alt-a").unwrap()));
        assert_eq!(5, modifier_code(&Keystroke::parse("ctrl-a").unwrap()));
        assert_eq!(6, modifier_code(&Keystroke::parse("shift-ctrl-a").unwrap()));
        assert_eq!(7, modifier_code(&Keystroke::parse("alt-ctrl-a").unwrap()));
        assert_eq!(
            8,
            modifier_code(&Keystroke::parse("shift-ctrl-alt-a").unwrap())
        );
    }
}
