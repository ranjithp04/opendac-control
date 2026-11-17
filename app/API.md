# UI <-> Rust contract (M2)

The one place where the Svelte UI (`app/ui`) and the Tauri backend (`app/src-tauri`) agree on names and shapes.

| Piece | File |
|---|---|
| This document | `app/API.md` |
| Rust types (snapshot, enums, errors, `g6-core` mapping) | `app/src-tauri/src/api.rs` |
| Rust commands (thin wrappers, run on the blocking pool) | `app/src-tauri/src/commands.rs` |
| Rust backend (session, snapshots, events; Tauri-free, tested on `g6-core`'s fake device) | `app/src-tauri/src/backend.rs` |
| TypeScript mirror (types) and the only `invoke` / `listen` wrapper (`call`, `on`) | `app/ui/src/lib/api.ts` |
| Browser mock backend (same commands / errors / events, used outside Tauri) | `app/ui/src/lib/mock.ts` |

Change all of them together. `api.rs` has a test that pins the snapshot's JSON keys.

Conventions:

- JSON, **camelCase** field names, lower-case enum strings.
- Command names are `snake_case`. `invoke('set_eq_band', { band: 3, gain: 1.5 })`: argument keys are camelCase (Tauri converts the Rust names).
- UI values, not device values: levels are 0..1, EQ gains are dB. The mic noise reduction level is 0..1 here and 0..0.5 on the device (`Feature::ui_scale`); the backend converts.
- EQ pre-amp: `eq.preamp`, -6..6 dB in 0.5 steps, per output (feature `eq_preamp`). Direct Mode leaves it alone. Presets include it (`g6-core`). Scout does not lock it, but the G6 sets it to 0 dB when Scout turns on and restores the previous value when Scout turns off (verified on hardware), so the snapshot shows 0 while Scout is on; profiles and re-applies leave it out while Scout is on.

## 1. State snapshot

`AppState` is the full state of everything the panels show. One type, always complete.

```ts
interface AppState {
  revision: number;            // grows with every snapshot, see "Ordering"
  connection: 'connected' | 'disconnected';
  firmware: string | null;     // "2.1.250903.1324"
  output: 'headphones' | 'speakers' | null;   // active output (jack), null until first read
  directMode: boolean;         // our Direct Mode
  sbx: boolean;
  scout: boolean;
  eq: { enabled: boolean; bands: number[]; preamp: number };  // 10 bands, dB, 31 Hz .. 16 kHz; preamp dB
  effects: {
    surround:    { enabled; level; distance };       // level 0..1, distance 10..300 step 0.5
    crystalizer: { enabled; level };
    bass:        { enabled; level };
    dialog:      { enabled; level };                 // Dialog+
    smartVolume: { enabled; level; mode: 'normal' | 'loud' | 'night' };
  };
  mic: {
    boost: 0 | 10 | 20 | 30 | null;                  // dB, shared by both outputs
    voiceClarity: boolean;
    noiseReduction: number;                          // 0..1
    eqEnabled: boolean;
    eqBands: number[];                               // 8 gains, dB, mic EQ bands 1..8 (frequencies unknown)
    aec: boolean;
    smartVolume: boolean;                            // recording Smart Volume
  };
  dacFilter: 'fast-min' | 'slow-min' | 'nos' | 'fast-lin' | 'slow-lin' | null;
  dolbyDecoder: 'full' | 'normal' | 'night' | null;  // Dolby Digital, optical in
  lighting: { enabled: boolean; color: string };     // LED; color is lower-case "rrggbb", no "#"
  presets: string[];                                 // sorted
  activePreset: string | null;                       // footer "Profile: X"
  eqCurves: { name: string; bands: number[]; preamp: number }[];  // user-saved EQ curves, saved order
  micEqCurves: { name: string; bands: number[] }[];               // user-saved mic EQ curves (8 bands), saved order
  autoSwitch: { headphones: string | null; speakers: string | null };  // profile applied when the output becomes headphones / speakers
  launchAtLogin: boolean;
  hotkeys: {                                         // global hotkeys, opt-in
    enabled: boolean;                                // off by default
    toggleDirectMode: { accelerator: string; error: string | null };
    nextProfile:      { accelerator: string; error: string | null };
    toggleSbx:        { accelerator: string; error: string | null };
  };
  mixer: Mixer | null;                               // G6 USB audio controls, see "Mixer"; null = hide the tab
}
```

Rules:

- Per-output values (EQ, effects, and the mic values except `boost`) are those of the **active output**. The UI labels them with `output` ("Headphones" / "Speakers"); the mic tab says "these are the headphones values".
- Unknown values take a neutral default: off, flat EQ, level 0, distance 10, `output` / `boost` / `dacFilter` / `dolbyDecoder` `null`, `lighting` off with color `ffffff`. They are only meaningful when `connection` is `connected`, or while last-known settings are shown on the disconnected screen.
- `connection: 'disconnected'` -> show the "Waiting for the G6" screen. The snapshot keeps the last known values; `activePreset`, `autoSwitch` and `launchAtLogin` stay valid.
- `directMode: true` -> grey out and disable the **EQ tab and Effects tab** (and the Scout / SBX controls). Show the "Direct Mode is on" banner. Mic, Device tab and presets list stay usable.
- `scout: true` -> EQ band sliders, EQ on/off, the EQ curve chips / "Save curve" and Smart Volume enabled/level are read-only (Scout owns them: the device writes its curve `2, 3, 3, -2, -2, 0, 3, 4, 3, 2` and Smart Volume 0.9). The device also sets the pre-amp to 0 dB (`eq.preamp` shows the device's value). The **pre-amp slider stays editable**: the backend does not lock it (the UI disables the "Suggest pre-amp" button while Scout is on, since the bands are then the device's curve). The G6 accepts a pre-amp write while Scout is on, and the written value is the one kept after Scout turns off (verified, `docs/protocol-g6.md` section 3 item 9). `sbx` is false while Scout is on (the device turns it off).
- Effect rows other than Smart Volume look dimmed while `sbx` is false (they have no effect then), but stay editable.
- `mic.noiseReduction` is only shown while `mic.voiceClarity` is on.

### Mixer

The G6's playback / monitoring / recording levels are USB Audio Class controls, not HID, so they are read and written through the OS audio API (`mixer.rs`; macOS CoreAudio in `mixer/coreaudio.rs`, Windows Core Audio / WASAPI in `mixer/wasapi.rs`), not `g6-core`. `mixer` is `null` when the OS offers no mixer (Linux) or the G6 audio device is not found: the UI hides the Mixer tab then (and falls back to the EQ tab if it was open).

```ts
interface Mixer {
  playback:   MixerLevel | null;   // output volume / mute (CoreAudio output scope; Windows: the render endpoint's master volume)
  monitoring: MixerLevel | null;   // inputs heard through the output (play-through scope)
  recording:  MixerLevel | null;   // level / mute of the selected source (input scope)
  recordingSource: { current: number; options: { id: number; name: string }[] } | null;
}
interface MixerLevel { volume: number; volumeDb: number | null; mute: boolean | null }  // volume 0..1 scalar, mean of the channels
```

A scope or control the OS does not offer is `null`. On the test G6 all three scopes, mutes and the four sources (`1` Line In, `2` External Mic, `3` S/PDIF In, `4` What U Hear; names come from the driver) are present; the per-input monitoring / recording rows are not (`docs/protocol-g6.md` 2.7). `volume` is CoreAudio's scalar, not dB; `volumeDb` is what the driver says it is. Writing sets both channels to the same value (one slider per scope; balance is not exposed). Mixer commands work in Direct Mode and while the HID side is disconnected (they never touch the G6's HID interface; the UI shows its disconnected screen then, so that matters only to the backend and other callers). Changes made elsewhere (volume knob, System Settings, another app) arrive as a normal `state-changed` (CoreAudio property listeners, no polling); a hint that changes nothing announces nothing. The mixer is re-read after every write, so the returned snapshot has the value the OS reports.

### Device info

Returned by `get_device_info`, not part of `AppState` (it does not change while connected). Read it when the connection becomes `connected` and show it in the Device tab ("About this device").

```ts
interface DeviceInfo {
  firmware: string | null;     // "2.1.250903.1324"
  serial: string | null;       // 8 bytes, 16 lowercase hex digits, e.g. "ef67740000000000"
  dspVersion: string | null;   // bytes as lowercase hex, e.g. "30011000"
  hardwareId: string | null;   // bytes as lowercase hex, e.g. "97000000"
}
```

### Ordering

Snapshots reach the UI from three places: command results, `state-changed`, `get_state`. They can arrive out of order. **Keep the snapshot with the highest `revision` and drop older ones.** While a slider is being dragged do not overwrite its value from an incoming snapshot; apply the snapshot on release.

Startup: `listen('state-changed')` first, then `get_state()`, then apply whichever has the higher `revision`. On `panel-shown` the UI may call `refresh_state()` if it wants the device re-read (optional; events keep it live without polling).

Sliders: send at most every 50 ms while dragging and once on release (design decision). Each call is applied and read back by `g6-core`, so keep one call in flight per control and send the latest value when it returns.

## 2. Commands

All commands are `async` and resolve with a value or reject with an `ApiError` (section 4). Every write command resolves with the **new `AppState`**, already containing the confirmed value (read back from the device).

Write commands reject `notConnected` until the session has synced after a connect (`connection` is `connected`). `g6-core` validates every value against its feature table before sending; a value is first rounded to the feature's step (e.g. 1.3 dB is written as 1.5 dB), then range-checked: a rounded value outside the range rejects `invalidValue` (the backend does not clamp, though a value within half a step of a limit rounds onto it).

| Command | Arguments | Resolves | `g6-core` call | Rejects (besides `notConnected`, `invalidValue`, `deviceTimeout`, `deviceRejected`, `deviceMismatch`) |
|---|---|---|---|---|
| `get_state` | none | `AppState` | session state, no device access | never rejects (works while disconnected) |
| `refresh_state` | none | `AppState` | `Session::refresh` | |
| `get_device_info` | none | `DeviceInfo` | `Device::firmware`, `serial`, `dsp_version`, `hw_id`; read once per connection and cached (reset on every connect), a field the device did not answer is `null` and is read again on the next call | never rejects. While disconnected it returns the cache without touching the device (all `null` if nothing was read). Shows the serial: never log it |
| **EQ, SBX, Scout** | | | | |
| `set_eq_enabled` | `enabled: boolean` | `AppState` | `write_feature(eq)` | `directModeActive`, `scoutActive` |
| `set_eq_band` | `band: 0..9`, `gain: number` (-12..12, step 0.5) | `AppState` | `write_feature(eq_31 .. eq_16k)` | `directModeActive`, `scoutActive` |
| `set_eq_bands` | `gains: number[10]` | `AppState` | 10 x `write_feature` | `directModeActive`, `scoutActive`; `invalidValue` unless exactly 10 values |
| `set_eq_preamp` | `db: number` (-6..6, step 0.5) | `AppState` | `write_feature(eq_preamp)` | `directModeActive` (allowed while Scout is on; the device shows 0 dB then) |
| `apply_eq_curve` | `bands: number[10]`, `preamp: number` | `AppState` | validates everything first, then 10 x `write_feature(eq_*)`, `write_feature(eq_preamp)`, `write_feature(eq, 1)`; on a failed write the device is re-read | `directModeActive`, `scoutActive`; `invalidValue` unless exactly 10 valid bands and a valid pre-amp |
| `set_sbx` | `enabled: boolean` | `AppState` | `set_mask(Sbx, enabled)`; turning SBX on while Scout is on switches Scout off first | `directModeActive` |
| `set_scout` | `enabled: boolean` | `AppState` | `set_mask(Scout, enabled)` | `directModeActive` |
| `import_eq` | `text: string` (AutoEq / Equalizer APO) | `EqImport` | `g6_core::eq_import::parse_eq_text`; pure, no device access, no Direct Mode / Scout check (the UI then applies the result with `apply_eq_curve`) | `invalidValue` (no `Preamp` / `Filter` / `GraphicEQ` line, a broken line named by number, text over 1 MiB) |
| **Effects** | | | | |
| `set_effect_enabled` | `effect: EffectName`, `enabled: boolean` | `AppState` | `write_feature(surround \| crystalizer \| bass \| dialog \| smartvol)` | `directModeActive`; `scoutActive` for `smartVolume` |
| `set_effect_level` | `effect: EffectName`, `level: number` (0..1) | `AppState` | `write_feature(<name>_level)` | `directModeActive`; `scoutActive` for `smartVolume` |
| `set_surround_distance` | `distance: number` (10..300, step 0.5) | `AppState` | `write_feature(surround_distance)` | `directModeActive` |
| `set_smart_volume_mode` | `mode: 'normal' \| 'loud' \| 'night'` | `AppState` | `write_feature(smartvol_mode)` (choice) | `directModeActive` |
| **Mic** | | | | |
| `set_mic_boost` | `db: 0 \| 10 \| 20 \| 30` | `AppState` | `set_mic_boost` | |
| `set_voice_clarity` | `enabled: boolean` | `AppState` | `write_feature(mic_clarity)` | |
| `set_noise_reduction` | `level: number` (0..1) | `AppState` | `write_feature(mic_nr_level)` with `from_ui` | |
| `set_mic_eq_enabled` | `enabled: boolean` | `AppState` | `write_feature(mic_eq)` | |
| `set_mic_eq_band` | `band: 0..7`, `gain: number` (-12..12, step 0.5) | `AppState` | `write_feature(mic_eq_1 .. mic_eq_8)` | `invalidValue` for a band outside 0..7 |
| `apply_mic_eq_curve` | `bands: number[8]` | `AppState` | validates everything first, then 8 x `write_feature(mic_eq_*)`, `write_feature(mic_eq, 1)`; on a failed write the device is re-read. Used by the built-in presets and saved mic curves | `invalidValue` unless exactly 8 valid bands. Allowed in Direct Mode and Scout (mic is outside both) |
| `list_mic_eq_curves` | none | `MicEqCurve[]` | reads `mic_eq_curves.json`; also refreshes the snapshot's `micEqCurves` (`state-changed` if it changed) | `storage`. Works while disconnected |
| `save_mic_eq_curve` | `name: string` | `AppState` | saves the active output's current 8 mic EQ bands (live session state, no device read) under `name` | `invalidValue` (empty / too long / control characters / built-in name / more than 32 curves), `storage`. Allowed in Direct Mode and Scout |
| `delete_mic_eq_curve` | `name: string` | `AppState` | removes the curve | `curveNotFound`, `storage`. Works while disconnected |
| `set_mic_aec` | `enabled: boolean` | `AppState` | `write_feature(mic_aec)` | |
| `set_mic_smart_volume` | `enabled: boolean` | `AppState` | `write_feature(mic_smartvol)` | |
| **Device** | | | | |
| `set_dac_filter` | `filter: DacFilter` | `AppState` | `set_dac_filter` | |
| `set_lighting_enabled` | `enabled: boolean` | `AppState` | `set_lighting_enabled` | |
| `set_lighting_color` | `color: string` (`rrggbb`; a leading `#` is tolerated) | `AppState` | `rgb_from_hex`, then `set_lighting_color` (static mode + colour; does not switch the LED on) | `invalidValue` for anything but 6 hex digits (nothing is sent) |
| `set_output` | `output: 'headphones' \| 'speakers'` | `AppState` | `set_output`, then `Session::refresh` so the per-output values in the snapshot are the new output's. Manual override only: the headphone jack still switches the output on the next plug / unplug, and the session does not re-apply it on reconnect. Allowed while Direct Mode is on (Direct Mode follows the output) | |
| `set_dolby_decoder` | `mode: 'full' \| 'normal' \| 'night'` | `AppState` | `write_feature(decoder_mode)` (choice) | |
| `set_direct_mode` | `enabled: boolean` | `AppState` | `DirectMode::enable` / `disable`, then `Session::refresh`; `disable` with nothing saved is a no-op that resolves (also while disconnected) | `incomplete` (some steps failed, saved state kept, retry), `storage`. Once the enable / disable step ran (success, `incomplete` or a device error) a `state-changed` with the current `directMode` is emitted; the no-op disable emits one only when the cached `directMode` was stale. The early `storage` / `notConnected` rejections emit none (nothing changed) |
| **Mixer** | | | | |
| `set_mixer_volume` | `scope: 'playback' \| 'monitoring' \| 'recording'`, `volume: number` (0..1) | `AppState` | OS audio API (no `g6-core`): volume scalar on every channel of the scope, then re-read | `invalidValue` (not finite / outside 0..1; nothing sent), `mixerUnavailable` (no mixer, G6 audio device not found, scope missing or read-only), `internal` (the OS refused; status in the message) |
| `set_mixer_mute` | `scope`, `mute: boolean` | `AppState` | mute on element 0 of the scope, then re-read | `mixerUnavailable` (also when the scope has no mute), `internal` |
| `set_mixer_source` | `id: number` (one of `mixer.recordingSource.options`) | `AppState` | data source selector of the input scope, then re-read. Changing the source may change the recording level the OS reports; the returned snapshot has the new value | `invalidValue` (unknown id), `mixerUnavailable`, `internal` |
| **User EQ curves** | | | | |
| `list_eq_curves` | none | `EqCurve[]` | reads `eq_curves.json`; also refreshes the snapshot's `eqCurves` (`state-changed` if it changed) | `storage`. Works while disconnected |
| `save_eq_curve` | `name: string` | `AppState` | saves the active output's current bands and pre-amp (live session state, no device read) under `name` | `directModeActive`, `scoutActive` (the bands would be the device's), `invalidValue` (empty / too long / control characters / built-in name / more than 32 curves), `storage` |
| `delete_eq_curve` | `name: string` | `AppState` | removes the curve | `curveNotFound`, `storage`. Works while disconnected |
| **Presets** | | | | |
| `list_presets` | none | `string[]` | `PresetStore::list` | `storage`. Works while disconnected |
| `save_preset` | `name: string` | `AppState` | `PresetStore::save_current(name, state)` with the live session state (no extra device read); sets `activePreset` | `directModeActive` (the device then shows Direct Mode's EQ/SBX values), `presetInvalidName`, `storage` |
| `apply_preset` | `name: string` | `AppState` | `PresetStore::load` + `preset::apply`, then `Session::refresh`; sets `activePreset` | `directModeActive`, `presetNotFound`, `presetNoProfile` (no profile for the active output), `presetInvalidName`, `storage` |
| `rename_preset` | `from: string, to: string` | `AppState` | loads `from`, saves it as `to`, deletes `from` (case-only renames delete first); `activePreset` follows | `presetNotFound`, `presetInvalidName` (bad name or `to` already exists, ignoring case), `storage`. Works while disconnected |
| `export_preset` | `name: string` | `string \| null` | `PresetStore::load` (validates), then the native Save dialog (`tauri-plugin-dialog`, called from Rust) and an atomic write of `Preset::to_json`. Resolves with the path written, `null` if the dialog was cancelled. The path comes from the dialog, never from the UI. Works while disconnected and in Direct Mode | `presetNotFound`, `presetInvalidName`, `invalidValue` (stored file no longer valid), `storage` |
| `import_preset` | `text: string, name: string` | `AppState` | `Preset::from_json` (size limit, parse, at least one profile, `validate`: allow-listed per-output features and ranges), then `PresetStore::save`. Nothing is applied and `activePreset` does not change. Works while disconnected and in Direct Mode | `invalidValue` (bad content, nothing saved), `presetInvalidName` (bad name, or a profile of that name exists, ignoring case: never replaced), `storage` |
| `delete_preset` | `name: string` | `AppState` | `PresetStore::delete`; clears `activePreset` if it was this one | `presetNotFound`, `presetInvalidName`, `storage`. Works while disconnected |
| `set_auto_switch` | `output: 'headphones' \| 'speakers', profile: string \| null` | `AppState` | sets or clears (`null`) the auto-switch rule for `output`, saved in `app_settings.json`. Only a setting: nothing is applied, and it works while disconnected and in Direct Mode | `presetNotFound`, `presetInvalidName`, `storage` |
| **App** | | | | |
| `set_launch_at_login` | `enabled: boolean` | `AppState` | autostart plugin `enable` / `disable`, then `is_enabled()` into `launchAtLogin`. Off by default; never enabled without this call | `autostart` (a `state-changed` with the actual value is still emitted) |
| `set_hotkeys` | `enabled: boolean`, `toggleDirectMode`, `nextProfile`, `toggleSbx`: `string` (accelerators; `""` = that action's default) | `AppState` | `Backend::set_hotkeys` (validate, save in `app_settings.json`), then `tauri-plugin-global-shortcut` (`unregister` of each shortcut the last pass registered, then `on_shortcut` per action when `enabled`), then `Backend::set_hotkey_status`. A shortcut the OS refuses (e.g. another app has it) does not reject: it is `hotkeys.<action>.error` in the resolved snapshot (and the `state-changed` after it) | `invalidValue` (not modifiers + one key, no Ctrl / Alt / Cmd, unknown key, two actions on one shortcut; nothing stored, nothing re-registered), `storage`. Works while disconnected and in Direct Mode |
| `set_app_theme` | `theme: 'light' \| 'dark'` | `null` | app-wide light / dark window chrome (`AppHandle::set_theme`) and window background; called by the theme button and at startup | never rejects |
| `hide_panel` | none | `null` | hides the window (not called by the UI at the moment) | never rejects |

`EffectName` is `'surround' | 'crystalizer' | 'bass' | 'dialog' | 'smartVolume'`. `DacFilter` is `'fast-min' | 'slow-min' | 'nos' | 'fast-lin' | 'slow-lin'`.

Argument values that do not deserialise (an unknown enum string, a negative `band`) reject with a plain string, not an `ApiError`. That is a UI bug, not a runtime condition.

Presets (`g6-core::preset`; shown as **Profiles** in the UI, to tell them apart from EQ curves): a preset holds the playback settings (EQ, effects, SBX / Scout) of both outputs, **not** mic, decoder, DAC filter or mic boost. Names are 1-64 characters from letters, digits, space, `-`, `_`, `.`, not starting with `.`. `save_preset` overwrites. `apply_preset` writes the profile of the active output only. The app stores `activePreset` and the auto-switch rules in its own settings file, `app_settings.json` in the config dir (`{ "activePreset": "Music", "autoSwitch": { "headphones": "Music", "speakers": null } }`; both keys are optional, so older files load). `list_presets` also refreshes the snapshot's `presets` (and emits `state-changed` if the list changed on disk).

Import / export: a profile file is the stored preset JSON (`{ "headphones": {...}, "speakers": {...} }`, feature name -> device value, see `g6-core::preset`), so `g6 preset export` / `import` and the app's Export / Import interoperate. The UI reads a chosen file itself (`<input type="file">`, `file.text()`) and sends the text; writing goes through the native Save dialog in `export_preset`, so the webview never gets a path to write. The UI derives the name from the file name; when `import_preset` rejects `presetInvalidName` it asks for another name.

EQ import (`import_eq`, the EQ tab's "Import EQ..."): `EqImport = { bands: number[10], preamp: number, notes: string[] }`. Understood lines: `Preamp: -5.2 dB`, `Filter N: ON PK|LSC|HSC Fc .. Hz Gain .. dB Q ..` (also `LS`/`HS`, `PEQ`) and `GraphicEQ: 31 -3; 62 1.5; ...`. `OFF` filters are skipped; other filter types and unknown lines are skipped with a note. Filters are evaluated as RBJ biquads at 48 kHz and summed in dB at the ten band centres, GraphicEQ points are interpolated over log frequency; the result is rounded to 0.5 dB and clamped to +-12 dB (pre-amp +-6 dB), with a note per clamped value. The G6's own band shapes are not documented, so this is an approximation (assumed: 48 kHz, nominal band centres, shelf Q 0.707 when absent). The UI applies the result with `apply_eq_curve` (so EQ turns on and `directModeActive` / `scoutActive` apply) and shows the notes; the "Import EQ..." button is disabled in Direct Mode and while Scout is on.

Auto-switch: when a jack switch moves the output to one that has a rule, the backend applies that profile with the same code as `apply_preset` (so it sets `activePreset` and writes the active output's profile). It fires only when the output differs from the one known before the event: a connect only records the output (nothing fires at startup or after a replug), a repeated push of the same output does nothing, and `set_output` (manual) does not fire it. It is skipped (logged) while Direct Mode is on. It runs on the forwarder thread, so the writes' own events queue behind it and cannot re-trigger it. A failure (`presetNoProfile` for that output, a missing profile) is sent as `device-error`. Deleting a profile clears the rules that name it, and renaming one updates them; a rule found naming a missing profile is cleared when it fires.

Global hotkeys (`hotkeys.rs`, `shortcuts.rs`): off by default, three actions: toggle Direct Mode, apply the next profile, toggle SBX. An accelerator is modifiers and one key joined by `+` (`Ctrl+Alt+Cmd+D`); the backend spells it canonically (modifier order Ctrl, Alt, Shift, Cmd / Super; `Cmd` on macOS, `Super` elsewhere; letters upper case, keys named as `KeyboardEvent.code` names them: `Space`, `ArrowUp`, `F5`, `Minus`, ...), requires Ctrl, Alt or Cmd / Super (a global shortcut on a plain key or Shift + key would take that key from every app), and rejects two actions on one shortcut. Defaults: `Ctrl+Alt+Cmd+D` / `P` / `S` on macOS, `Ctrl+Alt+Shift+D` / `P` / `S` elsewhere. Stored in `app_settings.json` under `hotkeys` (`{ "enabled": false, "toggleDirectMode": "...", "nextProfile": "...", "toggleSbx": "..." }`; every key optional, an unusable stored accelerator falls back to its default and the others are kept; if two actions then share a shortcut, all three are the defaults). A press (key down only) runs the same `Backend` code as the commands (`set_direct_mode`, `set_sbx`, `apply_preset`), reading the state it toggles or steps from under the command lock, so two quick presses are two toggles (or two profiles on); a failure (Direct Mode on for SBX, no profile for the output, ...) is a `device-error`. "Next profile" goes alphabetically after `activePreset`, wrapping, and skips a profile that has no part for the active output (`presetNoProfile`), so it never sticks. Registration is attempted at startup only when `enabled`, and again by every `set_hotkeys`; a failure is `hotkeys.<action>.error`, never fatal. The UI (Device tab) records a key combination in a field with `KeyboardEvent.code`; the backend's spelling is what it then shows. A registered shortcut is taken by the OS, so pressing it in that field runs the action instead of recording it.

Launch at login: macOS Launch Agent through `tauri-plugin-autostart`; `launchAtLogin` in the snapshot is the plugin's `is_enabled()`.

EQ curves: the ten built-in curves (Flat, Bass boost, Bass cut, Treble boost, Warm, V-shape, Loudness, Speech, Footsteps, Small speakers) live in the UI (`app/ui/src/lib/eqCurves.ts`); applying any curve, built-in or saved, is `apply_eq_curve(bands, preamp)`. A chip is shown selected when its bands and pre-amp equal the active output's current values (built-ins first); if none matches the UI shows a "Custom" chip as selected. User curves are in `eq_curves.json` in the config dir, next to `app_settings.json` (`{ "curves": [ { "name", "bands", "preamp" } ] }`, saved order, written atomically). Names: trimmed, 1-32 characters, no control characters, unique ignoring case (saving an existing name replaces that curve in place), not a built-in name or "Custom" (ignoring case), at most 32 curves. The ten built-in names are also listed in `eq_curves.rs` (`RESERVED_NAMES`); keep both lists in sync.

Mic EQ presets: the 11 built-in presets (`Preset 1` .. `Preset 11`, 8 gains each) live in the UI (`app/ui/src/lib/micEqCurves.ts`); applying any preset or saved mic curve is `apply_mic_eq_curve(bands)`, which also turns mic EQ on. The names are placeholders (the real ones are unknown) and the mic EQ band frequencies are unknown, so the UI labels bands 1-8 only. A chip is selected when its 8 gains equal the active output's current `mic.eqBands`; otherwise a "Custom" chip is. The values are stored per output by the device. User mic curves are in `mic_eq_curves.json` in the config dir (`{ "curves": [ { "name", "bands" } ] }`), with the same name rules and the same 32-curve limit as EQ curves, kept separate from them (so an EQ curve and a mic curve may share a name); reserved: `Preset 1` .. `Preset 11` and `Custom` (also listed in `mic_eq_curves.rs`, `RESERVED_NAMES`). Mic curves are not part of profiles.

Lighting (`g6-core`: `lighting_enabled`, `lighting_color`) is the device's own and is also kept in `last_known.json` and re-applied on reconnect; it is not part of profiles. The UI shows a colour picker with swatches and sends `set_lighting_color` while dragging (50 ms sender), without overwriting the control mid-drag.

Not exposed on purpose: Creative's own Direct Mode (`0x39`) as a separate switch (`g6-core` folds it into our Direct Mode), the volume knob itself (the OS handles it; the Mixer tab shows and sets the playback volume it moves), quit (tray menu only).

## 3. Events (Rust -> UI)

Names use `-`. Listen with `listen('state-changed', e => e.payload)`.

| Event | Payload | When |
|---|---|---|
| `state-changed` | `AppState` | After every change of the snapshot: device events (buttons, jack switch, writes by others), reconnect, preset / settings changes. Coalesced, at most one per ~30 ms. **Authoritative**: the UI needs nothing else |
| `connection-changed` | `{ connection, firmware }` | Device connected (after sync, re-apply, firmware read and Direct Mode follow-up) or lost. Emitted just before `state-changed`. `firmware` is read on each connect; it keeps the last value while disconnected |
| `output-changed` | `{ output: 'headphones' \| 'speakers' }` | Headphone jack switched the output. Emitted just before `state-changed` |
| `device-error` | `ApiError` | A failure the UI did not ask for: a tray menu item or global hotkey action failed, an auto-switch profile could not be applied, or Direct Mode could not be extended to, or its pending restore could not be finished on, the output that became active (jack switch or reconnect). Show as a transient notice. (Failed re-apply steps on reconnect are only logged: `g6-core`'s `Session` does not report them to the caller.) |
| `panel-shown` | `null` | The panel window was just shown (tray click or second launch) |

The backend owns the device session; the UI never polls.

## 4. Errors

A rejected command promise, and the `device-error` payload, is:

```ts
interface ApiError { code: ErrorCode; message: string }   // message is for display / logs
```

| `code` | Meaning | Source in `g6-core` |
|---|---|---|
| `notConnected` | G6 not connected (reconnect in progress) | `events::Error::NotConnected`, `Transport` |
| `directModeActive` | Our Direct Mode is on; EQ, SBX, Scout, effects and presets are read-only | checked by the app |
| `scoutActive` | Scout is on; EQ and Smart Volume belong to the device | checked by the app (`features::scout_owned`: EQ, bands, Smart Volume; not the pre-amp) |
| `invalidValue` | Out of range (after rounding to the step), not finite, wrong count, not in the allow-list, a bad EQ curve name or mic EQ curve name, or an import file / text that is not a valid profile / EQ | `events::Error::Protocol(Feature(_))`, `preset::Error::Invalid` |
| `deviceTimeout` | No reply from the device | `events::Error::Timeout` |
| `deviceRejected` | The device ACKed with status `80` / `81` / `83` | `events::Error::Rejected` |
| `deviceMismatch` | Read-back after the write did not match | `events::Error::Mismatch` |
| `incomplete` | Direct Mode: some steps failed; saved state is kept, retry | `direct::Error::Incomplete` |
| `presetNotFound` | No preset with that name | `preset::Error::NotFound` |
| `presetInvalidName` | Name not allowed | `preset::Error::BadName` |
| `presetNoProfile` | The preset has no profile for the active output | `preset::Error::NoProfile` |
| `curveNotFound` | No saved EQ curve (or mic EQ curve) with that name | checked by the app |
| `storage` | A settings / preset / EQ curves / mic EQ curves / Direct Mode file could not be read or written | `store::StoreError` |
| `autostart` | Launch at login could not be changed | autostart plugin |
| `mixerUnavailable` | The mixer control is not offered: no mixer on this OS, G6 audio device not found, scope / mute / selector missing or read-only | `mixer::MixerError::Unavailable` |
| `internal` | Bug or stopped device thread | `events::Error::Shutdown`, `direct::Error::NotActive` |
| `notImplemented` | No longer returned (all commands are implemented); kept while `api.ts` lists it | |

The mapping is `impl From<...> for ApiError` in `api.rs` (tested).

## 5. Backend behaviour the UI relies on

Implemented in `backend.rs`; listed here because the UI depends on them.

1. **One session.** The backend starts one `g6_core::session::Session` at launch, with `reapply_on_first_connect: true` (restore last settings on connect). `Connected` is announced only after the state is synced and re-applied.
2. **State.** A forwarder thread reads the session receiver and groups events into bursts (events less than 30 ms apart, a burst cut at 200 ms). Each burst rebuilds the snapshot from `Session::state()` (`AppState::from_device`), bumps `revision` and emits one `state-changed` (after any `connection-changed` / `output-changed`). Snapshots are built under one lock, so a higher `revision` never carries an older session state.
3. **Writes.** A command returns a snapshot that includes the confirmed value. `Session::state()` is updated by the session thread from the write's echo event and can lag the write's return, so the backend keeps the confirmed `Written::value` as a pending overlay applied to every snapshot until the session reports that value for the setting (the echo) or a value no pending write set (a change made elsewhere), the device reconnects or switches output, or 2 s pass. The late echo of an older write to the same setting (a slider drag, two curves in a row) does not end the newer write's overlay. Commands that touch the device are serialised (one at a time), so multi-step ones (SBX after Scout off, Direct Mode, presets, Direct Mode following the jack) never interleave.
4. **Direct Mode follows the jack.** On `OutputSwitched` (and on every connect) while Direct Mode is on, `DirectMode::enable` runs again (idempotent, adds the new output's entry and switches its EQ off), as documented in `g6-core::direct`. A failure is sent as `device-error`. Once Direct Mode is off, `g6-core` keeps the entries of the other outputs on disk until `disable` runs while that output is active; so on `OutputSwitched` / connect with Direct Mode off, if `DirectMode::pending_outputs` contains the active output, the backend runs `disable` (restoring that output's EQ enable and effect toggles), reports a failure as `device-error`, then re-reads the device. Which of the two it does is decided under the command lock from the Direct Mode file, so a `set_direct_mode` that ran just before is never undone. The `directMode` flag is cached from the Direct Mode file at startup and after every enable / disable.
5. **Scout.** `set_sbx(true)` with Scout on switches Scout off first (matches the mockup). `set_scout(true)` needs nothing extra: `Session` re-reads what Scout replaces (`features::scout_resets`: EQ, bands, Smart Volume and the pre-amp) after a Scout change, and the resulting snapshot carries Scout's EQ curve, Smart Volume and 0 dB pre-amp (and the restored values after Scout off).
6. **Blocking.** Device calls block on the device thread; every command runs on Tauri's blocking pool (`spawn_blocking`), never on the main thread. A single write waits at most for the ACK and read-back timeouts (`g6-core` defaults: 300 ms + 500 ms); `refresh_state`, Direct Mode and presets read every feature and can take longer.
7. **No home directory.** If the OS config / data dirs cannot be found, the session runs without persistence and preset / Direct Mode / settings calls reject `storage`.

## 6. Shell behaviour (implemented in this step)

- Tray icon (template image); left click shows the panel, right click opens the menu: **Show OpenDAC Control**, **Direct Mode**, **SBX**, **Scout Mode** and **Lighting** (check items, the check is the current state), **Output** (submenu, Headphones / Speakers, the active one checked), **Profiles** (submenu, one item per profile, the active one checked; "No profiles saved" when empty), **Quit**. The menu is a pure function of the snapshot (`tray_menu.rs`); `panel::sync_tray` runs on every `state-changed` and replaces the native menu only when the model differs (slider drags change nothing in it). Device items are greyed out while disconnected; SBX, Scout and Profiles also while Direct Mode is on. A click calls the same `Backend` methods as the commands (`set_direct_mode`, `set_sbx`, `set_scout`, `set_output`, `set_lighting_enabled`, `apply_preset`) on the blocking pool; item ids carry the value to set (`sbx:off`), not "toggle", so a click handled twice is harmless. A native check item flips itself when clicked, so a click clears the cached menu and the next snapshot (also after a failure, which is a `device-error` plus a fresh snapshot) rebuilds it: the check marks always show the real state. A snapshot older than the one the menu was built from is ignored. One menu-event handler serves the app menu and the tray (Tauri calls app-level handlers for tray clicks too).
- Global shortcut plugin (`tauri-plugin-global-shortcut`) registered with no handler of its own; shortcuts are registered from Rust (`shortcuts.rs`) only, so no capability entry is added (the plugin's JS commands stay denied).
- Window: a normal 1280 x 720 landscape window (dark title bar, not resizable; EQ, Effects and Mic side by side, a fourth column with Mixer / Output / Profiles / System tabs) with a Dock icon, shown at launch. Closing it hides it and the app keeps running; the Dock icon, the tray / menu bar icon (left click) and a second launch show it again. Quit only from the app menu (⌘Q) or the tray menu.
- Single instance: starting a second copy shows the panel of the running one.
- Dialog plugin (`tauri-plugin-dialog`) registered for the profile export Save dialog. It is only called from Rust (`export_preset`), so no capability entry is needed.
- Capability (`src-tauri/capabilities/default.json`, window `main`): only `core:event:allow-listen` and `core:event:allow-unlisten`. The UI's only Tauri JS APIs are `invoke` (`@tauri-apps/api/core`; the app's own commands need no entry) and `listen` / its unlisten function (`@tauri-apps/api/event`). Anything else (window, path, app, dialog JS) must be added here explicitly.
- Launch at login plugin registered; off until `set_launch_at_login(true)`.

## 7. Open points

- **Hardware buttons during Direct Mode.** If the SBX / Scout button is pressed while Direct Mode is on, `sbx` / `scout` turn true while `directMode` is also true. The UI keeps the controls greyed out; whether the backend should switch them off again is not decided.
- **Input Monitoring.** `docs/protocol-g6.md` §3 item 6: a packaged app may need the macOS Input Monitoring permission to open the HID interface. Not tested; check when the first signed build opens the G6.
- **Mixer on Windows.** Playback, recording, monitoring, mute and the recording source were tried on a G6 and work (2026-11-18); the topology was dumped (`docs/protocol-g6.md` 2.7). Unverified: that the change callbacks fire for a volume changed elsewhere, and that the selector changes which endpoint carries the audio for an app that is already recording. `monitoring` on Windows is the selected recording input's own volume and mute on its way to the speakers (absent for What U Hear), read and written as dB parts, so it is not announced when changed elsewhere. The selector, the input-to-endpoint mapping and monitoring were derived from a topology dump of a real G6.
- **Mixer on macOS.** Setting any mixer control (all untested on the device, see `docs/protocol-g6.md` 2.7), the CoreAudio change listeners firing on a knob turn or a replug, and whether macOS asks for microphone access when the input volume or source is set (the app never opens an audio stream) have not been verified.
