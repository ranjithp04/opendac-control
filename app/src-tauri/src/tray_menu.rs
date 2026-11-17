//! The tray / menu bar menu as plain data: built from an [`AppState`] snapshot,
//! turned into a native menu by `panel.rs`. Tauri-free, so the model and the
//! item ids are tested here.
//!
//! Item ids carry the value to set (`sbx:on`, `output:speakers`, ...), not
//! "toggle", so a click delivered twice does the same thing twice.

use crate::api::{AppState, Connection, Output};

/// Ids of the two items the shell handles itself.
pub const SHOW: &str = "show";
pub const QUIT: &str = "quit";

/// One entry of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// `checked: Some(_)` is a check item (shown with a check mark).
    Item {
        id: String,
        label: String,
        enabled: bool,
        checked: Option<bool>,
    },
    Separator,
    Submenu {
        label: String,
        enabled: bool,
        items: Vec<MenuEntry>,
    },
}

/// What a device-related menu item does (the backend runs it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    DirectMode(bool),
    Sbx(bool),
    Scout(bool),
    Output(Output),
    Lighting(bool),
    Profile(String),
}

impl TrayAction {
    pub fn id(&self) -> String {
        fn onoff(on: bool) -> &'static str {
            if on {
                "on"
            } else {
                "off"
            }
        }
        match self {
            TrayAction::DirectMode(on) => format!("direct-mode:{}", onoff(*on)),
            TrayAction::Sbx(on) => format!("sbx:{}", onoff(*on)),
            TrayAction::Scout(on) => format!("scout:{}", onoff(*on)),
            TrayAction::Output(Output::Headphones) => "output:headphones".into(),
            TrayAction::Output(Output::Speakers) => "output:speakers".into(),
            TrayAction::Lighting(on) => format!("lighting:{}", onoff(*on)),
            TrayAction::Profile(name) => format!("profile:{name}"),
        }
    }

    /// `None` for ids that are not device actions (`show`, `quit`, unknown).
    pub fn from_id(id: &str) -> Option<TrayAction> {
        let (kind, value) = id.split_once(':')?;
        let on = || match value {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        Some(match kind {
            "direct-mode" => TrayAction::DirectMode(on()?),
            "sbx" => TrayAction::Sbx(on()?),
            "scout" => TrayAction::Scout(on()?),
            "lighting" => TrayAction::Lighting(on()?),
            "output" => TrayAction::Output(match value {
                "headphones" => Output::Headphones,
                "speakers" => Output::Speakers,
                _ => return None,
            }),
            "profile" if !value.is_empty() => TrayAction::Profile(value.to_string()),
            _ => return None,
        })
    }
}

fn item(action: &TrayAction, label: &str, enabled: bool, checked: Option<bool>) -> MenuEntry {
    MenuEntry::Item {
        id: action.id(),
        label: label.to_string(),
        enabled,
        checked,
    }
}

/// The menu for a snapshot. Device items are greyed out while the G6 is not
/// connected; SBX, Scout and Profiles also while Direct Mode is on (it owns
/// them, as in the panels). Show and Quit always work.
pub fn build(state: &AppState) -> Vec<MenuEntry> {
    let connected = state.connection == Connection::Connected;
    let playback = connected && !state.direct_mode;
    let output_item = |o: Output, label: &str| {
        item(
            &TrayAction::Output(o),
            label,
            connected,
            Some(state.output == Some(o)),
        )
    };
    let mut profiles: Vec<MenuEntry> = state
        .presets
        .iter()
        .map(|name| {
            item(
                &TrayAction::Profile(name.clone()),
                name,
                playback,
                Some(state.active_preset.as_deref() == Some(name)),
            )
        })
        .collect();
    if profiles.is_empty() {
        profiles.push(MenuEntry::Item {
            id: "no-profiles".into(),
            label: "No profiles saved".into(),
            enabled: false,
            checked: None,
        });
    }
    vec![
        MenuEntry::Item {
            id: SHOW.into(),
            label: "Show OpenDAC Control".into(),
            enabled: true,
            checked: None,
        },
        MenuEntry::Separator,
        item(
            &TrayAction::DirectMode(!state.direct_mode),
            "Direct Mode",
            connected,
            Some(state.direct_mode),
        ),
        item(
            &TrayAction::Sbx(!state.sbx),
            "SBX",
            playback,
            Some(state.sbx),
        ),
        item(
            &TrayAction::Scout(!state.scout),
            "Scout Mode",
            playback,
            Some(state.scout),
        ),
        MenuEntry::Separator,
        MenuEntry::Submenu {
            label: "Output".into(),
            enabled: connected,
            items: vec![
                output_item(Output::Headphones, "Headphones"),
                output_item(Output::Speakers, "Speakers"),
            ],
        },
        item(
            &TrayAction::Lighting(!state.lighting.enabled),
            "Lighting",
            connected,
            Some(state.lighting.enabled),
        ),
        MenuEntry::Submenu {
            label: "Profiles".into(),
            enabled: true,
            items: profiles,
        },
        MenuEntry::Separator,
        MenuEntry::Item {
            id: QUIT.into(),
            label: "Quit OpenDAC Control".into(),
            enabled: true,
            checked: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> AppState {
        AppState {
            connection: Connection::Connected,
            output: Some(Output::Headphones),
            ..AppState::default()
        }
    }

    fn flat(entries: &[MenuEntry]) -> Vec<&MenuEntry> {
        let mut out = Vec::new();
        for e in entries {
            out.push(e);
            if let MenuEntry::Submenu { items, .. } = e {
                out.extend(flat(items));
            }
        }
        out
    }

    fn find<'a>(entries: &'a [MenuEntry], id: &str) -> &'a MenuEntry {
        flat(entries)
            .into_iter()
            .find(|e| matches!(e, MenuEntry::Item { id: i, .. } if i == id))
            .unwrap_or_else(|| panic!("no item {id}"))
    }

    fn enabled_checked(e: &MenuEntry) -> (bool, Option<bool>) {
        match e {
            MenuEntry::Item {
                enabled, checked, ..
            } => (*enabled, *checked),
            _ => panic!("not an item"),
        }
    }

    #[test]
    fn disconnected_greys_out_the_device_items_but_not_show_and_quit() {
        let m = build(&AppState::default());
        assert_eq!(enabled_checked(find(&m, SHOW)), (true, None));
        assert_eq!(enabled_checked(find(&m, QUIT)), (true, None));
        for id in [
            "direct-mode:on",
            "sbx:on",
            "scout:on",
            "lighting:on",
            "output:headphones",
            "output:speakers",
        ] {
            assert!(!enabled_checked(find(&m, id)).0, "{id} should be disabled");
        }
    }

    #[test]
    fn connected_state_sets_the_check_marks_and_the_values_to_set() {
        let mut s = connected();
        s.sbx = true;
        s.lighting.enabled = true;
        let m = build(&s);
        // The id is the value the click sets: SBX is on, so clicking turns it off.
        assert_eq!(enabled_checked(find(&m, "sbx:off")), (true, Some(true)));
        assert_eq!(enabled_checked(find(&m, "scout:on")), (true, Some(false)));
        assert_eq!(
            enabled_checked(find(&m, "direct-mode:on")),
            (true, Some(false))
        );
        assert_eq!(
            enabled_checked(find(&m, "lighting:off")),
            (true, Some(true))
        );
        assert_eq!(
            enabled_checked(find(&m, "output:headphones")),
            (true, Some(true))
        );
        assert_eq!(
            enabled_checked(find(&m, "output:speakers")),
            (true, Some(false))
        );
    }

    #[test]
    fn direct_mode_greys_out_sbx_scout_and_profiles() {
        let mut s = connected();
        s.direct_mode = true;
        s.presets = vec!["Music".into()];
        let m = build(&s);
        assert_eq!(
            enabled_checked(find(&m, "direct-mode:off")),
            (true, Some(true))
        );
        assert!(!enabled_checked(find(&m, "sbx:on")).0);
        assert!(!enabled_checked(find(&m, "scout:on")).0);
        assert!(!enabled_checked(find(&m, "profile:Music")).0);
        // Output and lighting are not Direct Mode's.
        assert!(enabled_checked(find(&m, "output:speakers")).0);
        assert!(enabled_checked(find(&m, "lighting:on")).0);
    }

    #[test]
    fn profiles_submenu_lists_presets_and_marks_the_active_one() {
        let mut s = connected();
        s.presets = vec!["Games".into(), "Music".into()];
        s.active_preset = Some("Music".into());
        let m = build(&s);
        assert_eq!(
            enabled_checked(find(&m, "profile:Games")),
            (true, Some(false))
        );
        assert_eq!(
            enabled_checked(find(&m, "profile:Music")),
            (true, Some(true))
        );
        // Without profiles the submenu holds one disabled note.
        let empty = build(&connected());
        assert_eq!(enabled_checked(find(&empty, "no-profiles")), (false, None));
    }

    #[test]
    fn menu_follows_state_only_where_it_shows_something() {
        // Slider drags must not change the menu (it is rebuilt only on change).
        let a = connected();
        let mut b = a.clone();
        b.revision = 99;
        b.eq.bands[3] = 4.0;
        b.effects.bass.level = 0.7;
        b.mic.boost = Some(20);
        assert_eq!(build(&a), build(&b));
        b.scout = true;
        assert_ne!(build(&a), build(&b));
    }

    #[test]
    fn action_ids_round_trip() {
        for a in [
            TrayAction::DirectMode(true),
            TrayAction::DirectMode(false),
            TrayAction::Sbx(true),
            TrayAction::Scout(false),
            TrayAction::Output(Output::Headphones),
            TrayAction::Output(Output::Speakers),
            TrayAction::Lighting(true),
            TrayAction::Profile("My profile-1.2_x".into()),
        ] {
            assert_eq!(TrayAction::from_id(&a.id()), Some(a.clone()), "{a:?}");
        }
        for not_an_action in [
            SHOW,
            QUIT,
            "no-profiles",
            "sbx",
            "sbx:maybe",
            "output:both",
            "profile:",
            "",
        ] {
            assert_eq!(
                TrayAction::from_id(not_an_action),
                None,
                "{not_an_action:?}"
            );
        }
    }

    #[test]
    fn every_item_id_of_a_menu_is_unique_and_parses_or_is_a_shell_id() {
        let mut s = connected();
        s.presets = vec!["A".into(), "B".into()];
        let m = build(&s);
        let ids: Vec<&str> = flat(&m)
            .into_iter()
            .filter_map(|e| match e {
                MenuEntry::Item { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        for id in ids {
            assert!(
                id == SHOW || id == QUIT || TrayAction::from_id(id).is_some(),
                "{id}"
            );
        }
    }
}
