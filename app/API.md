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
- EQ pre-amp: `eq.preamp`, -6..6 dB in 0.5 steps, per output (feature `eq_preamp`). Scout does not own it; Direct Mode leaves it alone. Presets include it (`g6-core`).

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
    eqEnabled: boolean;                              // 8 band gains not exposed
    aec: boolean;
    smartVolume: boolean;                            // recording Smart Volume
  };
  dacFilter: 'fast-min' | 'slow-min' | 'nos' | 'fast-lin' | 'slow-lin' | null;
  dolbyDecoder: 'full' | 'normal' | 'night' | null;  // Dolby Digital, optical in
  presets: string[];                                 // sorted
  activePreset: string | null;                       // footer "Profile: X"
  eqCurves: { name: string; bands: number[]; preamp: number }[];  // user-saved EQ curves, saved order
  launchAtLogin: boolean;
}
```

Rules:

- Per-output values (EQ, effects, and the mic values except `boost`) are those of the **active output**. The UI labels them with `output` ("Headphones" / "Speakers"); the mic tab says "these are the headphones values".
- Unknown values take a neutral default: off, flat EQ, level 0, distance 10, `output` / `boost` / `dacFilter` / `dolbyDecoder` `null`. They are only meaningful when `connection` is `connected`, or while last-known settings are shown on the disconnected screen.
- `connection: 'disconnected'` -> show the "Waiting for the G6" screen. The snapshot keeps the last known values; `activePreset` and `launchAtLogin` stay valid.
- `directMode: true` -> grey out and disable the **EQ tab and Effects tab** (and the Scout / SBX controls). Show the "Direct Mode is on" banner. Mic, Device tab and presets list stay usable.
- `scout: true` -> EQ band sliders, EQ on/off, the EQ curve chips / "Save curve" and Smart Volume enabled/level are read-only (Scout owns them: the device writes its curve `2, 3, 3, -2, -2, 0, 3, 4, 3, 2` and Smart Volume 0.9). The **pre-amp slider stays editable**: Scout does not own it. `sbx` is false while Scout is on (the device turns it off).
- Effect rows other than Smart Volume look dimmed while `sbx` is false (they have no effect then), but stay editable.
- `mic.noiseReduction` is only shown while `mic.voiceClarity` is on.

### Ordering

Snapshots reach the UI from three places: command results, `state-changed`, `get_state`. They can arrive out of order. **Keep the snapshot with the highest `revision` and drop older ones.** While a slider is being dragged do not overwrite its value from an incoming snapshot; apply the snapshot on release.

Startup: `listen('state-changed')` first, then `get_state()`, then apply whichever has the higher `revision`. On `panel-shown` the UI may call `refresh_state()` if it wants the device re-read (optional; events keep it live without polling).

Sliders: send at most every 50 ms while dragging and once on release (the project plan M2). Each call is applied and read back by `g6-core`, so keep one call in flight per control and send the latest value when it returns.

## 2. Commands

All commands are `async` and resolve with a value or reject with an `ApiError` (section 4). Every write command resolves with the **new `AppState`**, already containing the confirmed value (read back from the device).

Write commands reject `notConnected` until the session has synced after a connect (`connection` is `connected`). `g6-core` validates every value against its feature table before sending; out-of-range or off-step values reject `invalidValue` (the backend does not clamp).

| Command | Arguments | Resolves | `g6-core` call | Rejects (besides `notConnected`, `invalidValue`, `deviceTimeout`, `deviceRejected`, `deviceMismatch`) |
|---|---|---|---|---|
| `get_state` | none | `AppState` | session state, no device access | never rejects (works while disconnected) |
| `refresh_state` | none | `AppState` | `Session::refresh` | |
| **EQ, SBX, Scout** | | | | |
| `set_eq_enabled` | `enabled: boolean` | `AppState` | `write_feature(eq)` | `directModeActive`, `scoutActive` |
| `set_eq_band` | `band: 0..9`, `gain: number` (-12..12, step 0.5) | `AppState` | `write_feature(eq_31 .. eq_16k)` | `directModeActive`, `scoutActive` |
| `set_eq_bands` | `gains: number[10]` | `AppState` | 10 x `write_feature` | `directModeActive`, `scoutActive`; `invalidValue` unless exactly 10 values |
| `set_eq_preamp` | `db: number` (-6..6, step 0.5) | `AppState` | `write_feature(eq_preamp)` | `directModeActive` (allowed while Scout is on) |
| `apply_eq_curve` | `bands: number[10]`, `preamp: number` | `AppState` | validates everything first, then 10 x `write_feature(eq_*)`, `write_feature(eq_preamp)`, `write_feature(eq, 1)`; on a failed write the device is re-read | `directModeActive`, `scoutActive`; `invalidValue` unless exactly 10 valid bands and a valid pre-amp |
| `set_sbx` | `enabled: boolean` | `AppState` | `set_mask(Sbx, enabled)`; turning SBX on while Scout is on switches Scout off first | `directModeActive` |
| `set_scout` | `enabled: boolean` | `AppState` | `set_mask(Scout, enabled)` | `directModeActive` |
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
| `set_mic_aec` | `enabled: boolean` | `AppState` | `write_feature(mic_aec)` | |
| `set_mic_smart_volume` | `enabled: boolean` | `AppState` | `write_feature(mic_smartvol)` | |
| **Device** | | | | |
| `set_dac_filter` | `filter: DacFilter` | `AppState` | `set_dac_filter` | |
| `set_dolby_decoder` | `mode: 'full' \| 'normal' \| 'night'` | `AppState` | `write_feature(decoder_mode)` (choice) | |
| `set_direct_mode` | `enabled: boolean` | `AppState` | `DirectMode::enable` / `disable`, then `Session::refresh`; `disable` with nothing saved is a no-op that resolves (also while disconnected) | `incomplete` (some steps failed, saved state kept, retry), `storage`. On any outcome a `state-changed` with the current `directMode` is emitted |
| **User EQ curves** | | | | |
| `list_eq_curves` | none | `EqCurve[]` | reads `eq_curves.json`; also refreshes the snapshot's `eqCurves` (`state-changed` if it changed) | `storage`. Works while disconnected |
| `save_eq_curve` | `name: string` | `AppState` | saves the active output's current bands and pre-amp (live session state, no device read) under `name` | `directModeActive`, `scoutActive` (the bands would be the device's), `invalidValue` (empty / too long / control characters / built-in name / more than 32 curves), `storage` |
| `delete_eq_curve` | `name: string` | `AppState` | removes the curve | `curveNotFound`, `storage`. Works while disconnected |
| **Presets** | | | | |
| `list_presets` | none | `string[]` | `PresetStore::list` | `storage`. Works while disconnected |
| `save_preset` | `name: string` | `AppState` | `PresetStore::save_current(name, state)` with the live session state (no extra device read); sets `activePreset` | `directModeActive` (the device then shows Direct Mode's EQ/SBX values), `presetInvalidName`, `storage` |
| `apply_preset` | `name: string` | `AppState` | `PresetStore::load` + `preset::apply`, then `Session::refresh`; sets `activePreset` | `directModeActive`, `presetNotFound`, `presetNoProfile` (no profile for the active output), `presetInvalidName`, `storage` |
| `rename_preset` | `from: string, to: string` | `AppState` | loads `from`, saves it as `to`, deletes `from` (case-only renames delete first); `activePreset` follows | `presetNotFound`, `presetInvalidName` (bad name or `to` already exists, ignoring case), `storage`. Works while disconnected |
| `delete_preset` | `name: string` | `AppState` | `PresetStore::delete`; clears `activePreset` if it was this one | `presetNotFound`, `presetInvalidName`, `storage`. Works while disconnected |
| **App** | | | | |
| `set_launch_at_login` | `enabled: boolean` | `AppState` | autostart plugin `enable` / `disable`, then `is_enabled()` into `launchAtLogin`. Off by default; never enabled without this call | `autostart` (a `state-changed` with the actual value is still emitted) |
| `set_app_theme` | `theme: 'light' \| 'dark'` | `null` | app-wide light / dark window chrome (`AppHandle::set_theme`) and window background; called by the theme button and at startup | never rejects |
| `hide_panel` | none | `null` | hides the window (not called by the UI at the moment) | never rejects |

`EffectName` is `'surround' | 'crystalizer' | 'bass' | 'dialog' | 'smartVolume'`. `DacFilter` is `'fast-min' | 'slow-min' | 'nos' | 'fast-lin' | 'slow-lin'`.

Argument values that do not deserialise (an unknown enum string, a negative `band`) reject with a plain string, not an `ApiError`. That is a UI bug, not a runtime condition.

Presets (`g6-core::preset`; shown as **Profiles** in the UI, to tell them apart from EQ curves): a preset holds the playback settings (EQ, effects, SBX / Scout) of both outputs, **not** mic, decoder, DAC filter or mic boost. Names are 1-64 characters from letters, digits, space, `-`, `_`, `.`, not starting with `.`. `save_preset` overwrites. `apply_preset` writes the profile of the active output only. The app stores `activePreset` in its own settings file, `app_settings.json` in the config dir (`{ "activePreset": "Music" }`). `list_presets` also refreshes the snapshot's `presets` (and emits `state-changed` if the list changed on disk).

Launch at login: macOS Launch Agent through `tauri-plugin-autostart`; `launchAtLogin` in the snapshot is the plugin's `is_enabled()`.

EQ curves: the ten built-in curves (Flat, Bass boost, Bass cut, Treble boost, Warm, V-shape, Loudness, Speech, Footsteps, Small speakers) live in the UI (`app/ui/src/lib/eqCurves.ts`); applying any curve, built-in or saved, is `apply_eq_curve(bands, preamp)`. A chip is shown selected when its bands and pre-amp equal the active output's current values (built-ins first); if none matches the UI shows a "Custom" chip as selected. User curves are in `eq_curves.json` in the config dir, next to `app_settings.json` (`{ "curves": [ { "name", "bands", "preamp" } ] }`, saved order, written atomically). Names: trimmed, 1-32 characters, no control characters, unique ignoring case (saving an existing name replaces that curve in place), not a built-in name or "Custom" (ignoring case), at most 32 curves. The ten built-in names are also listed in `eq_curves.rs` (`RESERVED_NAMES`); keep both lists in sync.

Not exposed on purpose: the 8 mic EQ band gains, Creative's Direct Mode (`0x39`), volume / mute (the OS handles the knob), quit (tray menu only).

## 3. Events (Rust -> UI)

Names use `-`. Listen with `listen('state-changed', e => e.payload)`.

| Event | Payload | When |
|---|---|---|
| `state-changed` | `AppState` | After every change of the snapshot: device events (buttons, jack switch, writes by others), reconnect, preset / settings changes. Coalesced, at most one per ~30 ms. **Authoritative**: the UI needs nothing else |
| `connection-changed` | `{ connection, firmware }` | Device connected (after sync, re-apply, firmware read and Direct Mode follow-up) or lost. Emitted just before `state-changed`. `firmware` is read on each connect; it keeps the last value while disconnected |
| `output-changed` | `{ output: 'headphones' \| 'speakers' }` | Headphone jack switched the output. Emitted just before `state-changed` |
| `device-error` | `ApiError` | A failure the UI did not ask for: Direct Mode could not be extended to, or its pending restore could not be finished on, the output that became active (jack switch or reconnect). Show as a transient notice. (Failed re-apply steps on reconnect are only logged: `g6-core`'s `Session` does not report them to the caller.) |
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
| `scoutActive` | Scout is on; EQ and Smart Volume belong to the device | checked by the app (`features::scout_owned`) |
| `invalidValue` | Out of range, off-step, not finite, wrong count, not in the allow-list, or a bad EQ curve name | `events::Error::Protocol(Feature(_))`, `preset::Error::Invalid` |
| `deviceTimeout` | No reply from the device | `events::Error::Timeout` |
| `deviceRejected` | The device ACKed with status `80` / `81` / `83` | `events::Error::Rejected` |
| `deviceMismatch` | Read-back after the write did not match | `events::Error::Mismatch` |
| `incomplete` | Direct Mode: some steps failed; saved state is kept, retry | `direct::Error::Incomplete` |
| `presetNotFound` | No preset with that name | `preset::Error::NotFound` |
| `presetInvalidName` | Name not allowed | `preset::Error::BadName` |
| `presetNoProfile` | The preset has no profile for the active output | `preset::Error::NoProfile` |
| `curveNotFound` | No saved EQ curve with that name | checked by the app |
| `storage` | A settings / preset / EQ curves / Direct Mode file could not be read or written | `store::StoreError` |
| `autostart` | Launch at login could not be changed | autostart plugin |
| `internal` | Bug or stopped device thread | `events::Error::Shutdown`, `direct::Error::NotActive` |
| `notImplemented` | No longer returned (all commands are implemented); kept while `api.ts` lists it | |

The mapping is `impl From<...> for ApiError` in `api.rs` (tested).

## 5. Backend behaviour the UI relies on

Implemented in `backend.rs`; listed here because the UI depends on them.

1. **One session.** The backend starts one `g6_core::session::Session` at launch, with `reapply_on_first_connect: true` (the project plan M2: restore last settings on connect). `Connected` is announced only after the state is synced and re-applied.
2. **State.** A forwarder thread reads the session receiver and groups events into bursts (events less than 30 ms apart, a burst cut at 200 ms). Each burst rebuilds the snapshot from `Session::state()` (`AppState::from_device`), bumps `revision` and emits one `state-changed` (after any `connection-changed` / `output-changed`). Snapshots are built under one lock, so a higher `revision` never carries an older session state.
3. **Writes.** A command returns a snapshot that includes the confirmed value. `Session::state()` is updated by the session thread from the write's echo event and can lag the write's return, so the backend keeps the confirmed `Written::value` as a pending overlay applied to every snapshot until the session reports any value for that setting (the echo or a later change), the device reconnects or switches output, or 2 s pass. Commands that touch the device are serialised (one at a time), so multi-step ones (SBX after Scout off, Direct Mode, presets, Direct Mode following the jack) never interleave.
4. **Direct Mode follows the jack.** On `OutputSwitched` (and on every connect) while Direct Mode is on, `DirectMode::enable` runs again (idempotent, adds the new output's entry and switches its EQ off), as documented in `g6-core::direct`. A failure is sent as `device-error`. Once Direct Mode is off, `g6-core` keeps the entries of the other outputs on disk until `disable` runs while that output is active; so on `OutputSwitched` / connect with Direct Mode off, if `DirectMode::pending_outputs` contains the active output, the backend runs `disable` (restoring that output's EQ enable and effect toggles), reports a failure as `device-error`, then re-reads the device. The `directMode` flag is cached from the Direct Mode file at startup and after every enable / disable.
5. **Scout.** `set_sbx(true)` with Scout on switches Scout off first (matches the mockup). `set_scout(true)` needs nothing extra: `Session` re-reads the Scout-owned features after a Scout change, and the resulting snapshot carries Scout's EQ curve and Smart Volume.
6. **Blocking.** Device calls block on the device thread; every command runs on Tauri's blocking pool (`spawn_blocking`), never on the main thread. A single write waits at most for the ACK and read-back timeouts (`g6-core` defaults: 300 ms + 500 ms); `refresh_state`, Direct Mode and presets read every feature and can take longer.
7. **No home directory.** If the OS config / data dirs cannot be found, the session runs without persistence and preset / Direct Mode / settings calls reject `storage`.

## 6. Shell behaviour (implemented in this step)

- Tray icon (template image); left click shows / hides the panel, right click opens a menu with **Quit**.
- Window: a normal 380 x 720 window (dark title bar, not resizable) with a Dock icon, shown at launch. Closing it hides it and the app keeps running; the Dock icon, the tray / menu bar icon (left click) and a second launch show it again. Quit only from the app menu (⌘Q) or the tray menu.
- Single instance: starting a second copy shows the panel of the running one.
- Launch at login plugin registered; off until `set_launch_at_login(true)`.

## 7. Open points

- **Hardware buttons during Direct Mode.** If the SBX / Scout button is pressed while Direct Mode is on, `sbx` / `scout` turn true while `directMode` is also true. The UI keeps the controls greyed out; whether the backend should switch them off again is not decided.
- **Input Monitoring.** the project plan section 5 item 6: a packaged app may need the macOS Input Monitoring permission to open the HID interface. Not tested; check when the first signed build opens the G6.
