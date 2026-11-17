//! Global hotkey settings: the three actions, their accelerators and the
//! validation. Tauri-free (the registration with the OS is in `shortcuts.rs`);
//! stored in `app_settings.json` by the backend. Off by default.
//!
//! An accelerator is modifiers and one key joined with `+`, for example
//! `Ctrl+Alt+Cmd+D`. [`normalize`] turns what the user typed (or the UI
//! captured) into one canonical spelling that `tauri-plugin-global-shortcut`
//! parses, so equal shortcuts compare equal.

use serde::{Deserialize, Serialize};

/// What a hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleDirectMode,
    NextProfile,
    ToggleSbx,
}

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 3] = [
        HotkeyAction::ToggleDirectMode,
        HotkeyAction::NextProfile,
        HotkeyAction::ToggleSbx,
    ];

    /// Position in [`Self::ALL`] (the order of the status array).
    pub fn index(self) -> usize {
        match self {
            HotkeyAction::ToggleDirectMode => 0,
            HotkeyAction::NextProfile => 1,
            HotkeyAction::ToggleSbx => 2,
        }
    }

    /// Name shown in error messages.
    pub fn label(self) -> &'static str {
        match self {
            HotkeyAction::ToggleDirectMode => "Toggle Direct Mode",
            HotkeyAction::NextProfile => "Next profile",
            HotkeyAction::ToggleSbx => "Toggle SBX",
        }
    }

    /// Suggested accelerator. Ctrl+Alt+Cmd on macOS, Ctrl+Alt+Shift elsewhere
    /// (Win+Alt combinations are partly taken by the system there): three
    /// modifiers, so they rarely collide with other apps.
    pub fn default_accelerator(self) -> String {
        let key = match self {
            HotkeyAction::ToggleDirectMode => "D",
            HotkeyAction::NextProfile => "P",
            HotkeyAction::ToggleSbx => "S",
        };
        if cfg!(target_os = "macos") {
            format!("Ctrl+Alt+{META}+{key}")
        } else {
            format!("Ctrl+Alt+Shift+{key}")
        }
    }
}

/// Canonical spelling of the Command / Windows key (Cmd on macOS, where the
/// key is called that; Super elsewhere). Both parse to the same modifier.
const META: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Super"
};

/// The hotkey part of `app_settings.json`. Every field has a serde default, so
/// older files (no `hotkeys` key, or only some fields) load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HotkeySettings {
    pub enabled: bool,
    pub toggle_direct_mode: String,
    pub next_profile: String,
    pub toggle_sbx: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        HotkeySettings {
            enabled: false,
            toggle_direct_mode: HotkeyAction::ToggleDirectMode.default_accelerator(),
            next_profile: HotkeyAction::NextProfile.default_accelerator(),
            toggle_sbx: HotkeyAction::ToggleSbx.default_accelerator(),
        }
    }
}

impl HotkeySettings {
    pub fn accelerator(&self, action: HotkeyAction) -> &str {
        match action {
            HotkeyAction::ToggleDirectMode => &self.toggle_direct_mode,
            HotkeyAction::NextProfile => &self.next_profile,
            HotkeyAction::ToggleSbx => &self.toggle_sbx,
        }
    }

    fn accelerator_mut(&mut self, action: HotkeyAction) -> &mut String {
        match action {
            HotkeyAction::ToggleDirectMode => &mut self.toggle_direct_mode,
            HotkeyAction::NextProfile => &mut self.next_profile,
            HotkeyAction::ToggleSbx => &mut self.toggle_sbx,
        }
    }

    /// The settings to use and store: every accelerator normalised (an empty
    /// one means the default), and no two actions on the same shortcut.
    pub fn validated(&self) -> Result<HotkeySettings, String> {
        let mut out = self.clone();
        for action in HotkeyAction::ALL {
            *out.accelerator_mut(action) = self.normalized(action)?;
        }
        out.check_distinct()?;
        Ok(out)
    }

    /// One accelerator normalised (empty: the default).
    fn normalized(&self, action: HotkeyAction) -> Result<String, String> {
        let raw = self.accelerator(action).trim();
        if raw.is_empty() {
            Ok(action.default_accelerator())
        } else {
            normalize(raw).map_err(|e| format!("{}: {e}", action.label()))
        }
    }

    fn check_distinct(&self) -> Result<(), String> {
        for (i, a) in HotkeyAction::ALL.iter().enumerate() {
            for b in &HotkeyAction::ALL[i + 1..] {
                if self.accelerator(*a) == self.accelerator(*b) {
                    return Err(format!(
                        "{} and {} cannot use the same shortcut ({}).",
                        a.label(),
                        b.label(),
                        self.accelerator(*a)
                    ));
                }
            }
        }
        Ok(())
    }

    /// Like [`Self::validated`] for a file read from disk, but never fails
    /// (the app must still start): an unusable accelerator is replaced by
    /// its default, the others are kept; only if two actions then share a
    /// shortcut are all three the defaults. `enabled` is kept.
    pub fn sanitized(self) -> HotkeySettings {
        let mut out = self.clone();
        for action in HotkeyAction::ALL {
            *out.accelerator_mut(action) = self.normalized(action).unwrap_or_else(|e| {
                log::warn!("ignoring the stored hotkey: {e}");
                action.default_accelerator()
            });
        }
        if let Err(e) = out.check_distinct() {
            log::warn!("ignoring the stored hotkeys: {e}");
            return HotkeySettings {
                enabled: self.enabled,
                ..HotkeySettings::default()
            };
        }
        out
    }
}

/// Canonical spelling of an accelerator: modifiers in the order Ctrl, Alt,
/// Shift, Cmd / Super, then the key. At least one of Ctrl, Alt and Cmd /
/// Super is required (a global shortcut on a plain key or Shift+key would
/// take the key from every other app).
pub fn normalize(text: &str) -> Result<String, String> {
    let (mut ctrl, mut alt, mut shift, mut meta) = (false, false, false, false);
    let mut key: Option<String> = None;
    for raw in text.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            return Err(format!("\"{}\" has an empty part.", text.trim()));
        }
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "opt" => alt = true,
            "shift" => shift = true,
            "cmd" | "command" | "super" | "meta" | "win" | "windows" => meta = true,
            _ => {
                let k = canonical_key(token)
                    .ok_or_else(|| format!("\"{token}\" is not a key that can be used."))?;
                if key.replace(k).is_some() {
                    return Err(format!("\"{}\" has more than one key.", text.trim()));
                }
            }
        }
    }
    let Some(key) = key else {
        return Err(format!("\"{}\" has no key.", text.trim()));
    };
    if !(ctrl || alt || meta) {
        return Err(format!(
            "\"{}\" needs Ctrl, Alt or {META} as well.",
            text.trim()
        ));
    }
    let mut parts: Vec<&str> = Vec::new();
    for (on, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (meta, META)] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&key);
    Ok(parts.join("+"))
}

/// The key names are the ones the plugin's parser takes (and the ones
/// `KeyboardEvent.code` uses for these keys, apart from letters and digits).
fn canonical_key(token: &str) -> Option<String> {
    let t = token.to_ascii_lowercase();
    if t.len() == 1 {
        let c = t.chars().next()?;
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase().to_string());
        }
    }
    if let Some(n) = t.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=24).contains(&n) && t == format!("f{n}") {
            return Some(format!("F{n}"));
        }
    }
    let name = match t.as_str() {
        "space" => "Space",
        "tab" => "Tab",
        "enter" | "return" => "Enter",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "arrowup" | "up" => "ArrowUp",
        "arrowdown" | "down" => "ArrowDown",
        "arrowleft" | "left" => "ArrowLeft",
        "arrowright" | "right" => "ArrowRight",
        "-" | "minus" => "Minus",
        "=" | "equal" => "Equal",
        "," | "comma" => "Comma",
        "." | "period" => "Period",
        "/" | "slash" => "Slash",
        ";" | "semicolon" => "Semicolon",
        "'" | "quote" => "Quote",
        "`" | "backquote" => "Backquote",
        "\\" | "backslash" => "Backslash",
        "[" | "bracketleft" => "BracketLeft",
        "]" | "bracketright" => "BracketRight",
        _ => return None,
    };
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_orders_modifiers_and_spells_keys() {
        let m = META;
        assert_eq!(normalize("ctrl+alt+d").unwrap(), "Ctrl+Alt+D");
        assert_eq!(
            normalize("D + Command + Option + Control").unwrap(),
            format!("Ctrl+Alt+{m}+D")
        );
        assert_eq!(normalize("Alt+Shift+f5").unwrap(), "Alt+Shift+F5");
        assert_eq!(normalize("cmd+win+1").unwrap(), format!("{m}+1"));
        assert_eq!(normalize("Ctrl+Alt+up").unwrap(), "Ctrl+Alt+ArrowUp");
        assert_eq!(normalize("Ctrl+Alt+Space").unwrap(), "Ctrl+Alt+Space");
        assert_eq!(normalize("Ctrl+Alt+,").unwrap(), "Ctrl+Alt+Comma");
        // Idempotent.
        let once = normalize("alt+ctrl+Q").unwrap();
        assert_eq!(normalize(&once).unwrap(), once);
    }

    #[test]
    fn normalize_rejects_what_cannot_be_registered_safely() {
        for bad in [
            "",
            "+",
            "Ctrl+",
            "Ctrl+Alt",
            "D",
            "Shift+D",
            "Ctrl+Alt+D+P",
            "Ctrl+Alt+Escape",
            "Ctrl+Alt+F25",
            "Ctrl+Alt+F0",
            "Ctrl+Alt+F01",
            "Ctrl+Alt+Pause",
            "Ctrl+Alt+é",
            "Hyper+Ctrl+D",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn defaults_are_valid_distinct_and_off() {
        let d = HotkeySettings::default();
        assert!(!d.enabled);
        assert_eq!(d.validated().unwrap(), d);
        for a in HotkeyAction::ALL {
            assert_eq!(
                normalize(&a.default_accelerator()).unwrap(),
                d.accelerator(a)
            );
        }
        if cfg!(target_os = "macos") {
            assert_eq!(d.toggle_direct_mode, "Ctrl+Alt+Cmd+D");
        }
    }

    #[test]
    fn validated_fills_empty_normalises_and_rejects_duplicates() {
        let s = HotkeySettings {
            enabled: true,
            toggle_direct_mode: " alt+ctrl+x ".into(),
            next_profile: String::new(),
            toggle_sbx: "ctrl+alt+Y".into(),
        };
        let v = s.validated().unwrap();
        assert!(v.enabled);
        assert_eq!(v.toggle_direct_mode, "Ctrl+Alt+X");
        assert_eq!(
            v.next_profile,
            HotkeyAction::NextProfile.default_accelerator()
        );
        assert_eq!(v.toggle_sbx, "Ctrl+Alt+Y");

        let dup = HotkeySettings {
            toggle_direct_mode: "Ctrl+Alt+X".into(),
            toggle_sbx: "alt+ctrl+x".into(),
            ..HotkeySettings::default()
        };
        let e = dup.validated().unwrap_err();
        assert!(
            e.contains("Toggle Direct Mode") && e.contains("Toggle SBX"),
            "{e}"
        );

        let bad = HotkeySettings {
            next_profile: "P".into(),
            ..HotkeySettings::default()
        };
        assert!(bad.validated().unwrap_err().starts_with("Next profile:"));
    }

    #[test]
    fn stored_settings_load_with_defaults_and_bad_ones_fall_back() {
        // Older file: no hotkeys at all, or only some fields.
        let none: HotkeySettings = serde_json::from_str("{}").unwrap();
        assert_eq!(none, HotkeySettings::default());
        let some: HotkeySettings =
            serde_json::from_str(r#"{ "enabled": true, "toggleSbx": "Ctrl+Alt+K" }"#).unwrap();
        assert!(some.enabled);
        assert_eq!(some.toggle_sbx, "Ctrl+Alt+K");
        assert_eq!(
            some.toggle_direct_mode,
            HotkeySettings::default().toggle_direct_mode
        );

        let broken = HotkeySettings {
            enabled: true,
            toggle_sbx: "nonsense".into(),
            ..HotkeySettings::default()
        };
        let fixed = broken.sanitized();
        assert!(fixed.enabled);
        assert_eq!(
            fixed.toggle_sbx,
            HotkeyAction::ToggleSbx.default_accelerator()
        );

        // Only the unusable one falls back; the user's others are kept.
        let one_bad = HotkeySettings {
            enabled: true,
            toggle_direct_mode: "ctrl+alt+x".into(),
            next_profile: "Ctrl+Alt+Y".into(),
            toggle_sbx: "Ctrl+Alt+Escape".into(),
        };
        let fixed = one_bad.sanitized();
        assert!(fixed.enabled);
        assert_eq!(fixed.toggle_direct_mode, "Ctrl+Alt+X");
        assert_eq!(fixed.next_profile, "Ctrl+Alt+Y");
        assert_eq!(
            fixed.toggle_sbx,
            HotkeyAction::ToggleSbx.default_accelerator()
        );

        // A fallback that collides with another action: all defaults.
        let clash = HotkeySettings {
            enabled: true,
            toggle_direct_mode: "nonsense".into(),
            next_profile: HotkeyAction::ToggleDirectMode.default_accelerator(),
            toggle_sbx: "Ctrl+Alt+Y".into(),
        };
        assert_eq!(
            clash.sanitized(),
            HotkeySettings {
                enabled: true,
                ..HotkeySettings::default()
            }
        );
    }

    #[test]
    fn wire_shape_is_camel_case() {
        let v = serde_json::to_value(HotkeySettings::default()).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["enabled", "nextProfile", "toggleDirectMode", "toggleSbx"]
        );
    }
}
