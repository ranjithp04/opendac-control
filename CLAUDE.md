# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

OpenDAC Control: open-source replacement for Sound Blaster Command, for the Creative Sound BlasterX G6 USB DAC (`041e:3256`). It talks to the device over USB HID. Cargo workspace plus a Svelte UI.

## Commands

```sh
cargo build --workspace
cargo test --workspace                                  # no device needed (uses a fake device)
cargo test -p g6-core --test protocol_frames            # one integration-test file
cargo test -p g6-core <name>                            # one test by name filter
cargo clippy --workspace && cargo fmt --check

cargo test -p g6-core --test hardware -- --ignored --test-threads=1   # needs a real G6 plugged in
cargo run -p g6-cli -- status                           # `g6` CLI (also opens the real device)

cd app/ui && npm install                                # once
npm run check                                           # svelte-check (UI type check)
npm run dev                                             # UI alone at localhost:1420 against an in-memory mock, no device
npm run tauri dev                                       # full app, opens the real G6
npm run tauri build                                     # release bundles (.app/.dmg, or NSIS/MSI on Windows)
```

`cargo build -p opendac-control` works without Node: `app/src-tauri/build.rs` creates an empty `app/ui/dist` if the UI was not built. Don't run the hardware tests or `tauri dev` / `g6` commands unless the user wants the device touched. CI (`.github/workflows/build.yml`) runs fmt, clippy, `cargo test --workspace` and `npm run check` on macOS and Windows, then builds the unsigned bundles, only on `v*` tags or manual dispatch.

## Architecture

Four layers; all HID access goes through `g6-core` (the Mixer tab uses CoreAudio on macOS and WASAPI on Windows, from `app/src-tauri/src/mixer/`, not HID):

1. **`crates/g6-core`** (library). Bottom-up:
   - `transport.rs`: hidapi behind `Transport` / `Connector` traits; `transport/fake.rs` is the fake device all tests use.
   - `protocol.rs`: frame encode/decode (`5a CMD LEN PAYLOAD`, padded to 64 bytes, f32 little-endian) and push parsing.
   - `events.rs`: `Device` owns a reader thread. Calls are sent to it over a channel and block for the reply; replies are matched by command code (plus family/id for feature reads). Everything else the device sends is a push and becomes an `Event`. The thread reconnects with backoff on disconnect.
   - `features.rs`: the feature table, which is an allow-list (family/id, kind, range). Values are validated and rounded before sending.
   - `state.rs` (live `DeviceState`), `direct.rs` (Direct Mode), `preset.rs` (profiles), `store.rs` (paths and JSON files on disk).
   - `session.rs`: ties a `Device` to a live `DeviceState`, persists `last_known.json` and re-applies it on reconnect (not on the first connect, by default).
2. **`crates/g6-cli`** (`g6` binary, clap). `cli.rs` parses arguments, `action.rs` and `run.rs` execute them, `format.rs` prints output. Exit status 2 means bad input, 1 means a device or file error. Tests are in `crates/g6-cli/tests/cli.rs`.
3. **`app/src-tauri`** (Tauri 2 shell, crate `opendac-control`). `backend.rs` wraps one long-lived `Session` plus app state (Direct Mode, profiles, active profile, launch at login) and produces `AppState` snapshots. It is Tauri-free and tested on the fake device. Events leave through a `Sink` trait, implemented in `lib.rs`. `commands.rs` holds thin Tauri commands that run on the blocking pool. `api.rs` holds the wire types. `eq_curves.rs` stores user EQ curves.
4. **`app/ui`** (Svelte 5 + TypeScript + Vite). `lib/api.ts` is the only code that calls `invoke` / `listen`. `lib/store.svelte.ts` keeps the latest snapshot. `lib/mock.ts` is the browser-only backend.

**The UI/Rust contract is `app/API.md`.** Changing a command, event or snapshot field means changing `app/API.md`, `api.rs`, `commands.rs`, `ui/src/lib/api.ts` and `ui/src/lib/mock.ts` together (`api.rs` has a test pinning the snapshot's JSON keys). The UI keeps the snapshot with the highest `revision`, and doesn't overwrite a slider mid-drag.

Device behaviour that shapes the code (details in `docs/protocol-g6.md`):

- The G6 keeps separate EQ, effects and mic settings per output (headphones/speakers) and switches output on the jack automatically.
- Scout Mode owns the EQ bands and Smart Volume. It also sets the EQ pre-amp to 0 dB and restores it when Scout turns off (`docs/protocol-g6.md` section 1), so profiles and re-applies leave the pre-amp out while Scout is on (`features::scout_resets`); a user pre-amp write is still allowed and becomes the value kept after Scout turns off. Turning Scout on turns SBX off.
- The G6 may come back from a USB replug with settings reset, hence the re-apply logic.
- Direct Mode saves SBX/Scout/EQ/effects state to disk before turning them off, and never touches mic settings.
- The first write after opening the device gets no ACK; the read-back decides.

## Rules

- **Never open HID interface 3** (the volume knob). Opening it stops macOS from receiving the knob until a replug. Only interface 4 is opened.
- **Only write registers in the `features.rs` allow-list**, with range checks before anything reaches the device. New f32 settings (`5a 12`) go through the feature table. The other write commands (`0x26` SBX/Scout mask, `0x2c` output, `0x6c` DAC filter, `0x3c` mic boost, `0x3a` lighting, `0x39` hardware Direct Mode) are built only by typed builders in `protocol.rs` with fixed payloads and enum / bool / `Rgb` arguments; a new non-f32 command follows that pattern. `Device` exposes no raw-frame write. The Mixer's CoreAudio / WASAPI writes check their own 0..1 range and source ids.
- Protocol facts verified on hardware belong in `docs/protocol-g6.md`. The code doesn't depend on `soundblaster-x-g6-cli` (GPL); the MIT-licensed `linuxblaster_control` notes are credited in the README.
- `PLAN.md` is untracked, so don't reference it from tracked files.
- Windows paths for stored settings follow the `directories` crate; the README lists where each file lives.
- Project subagents are in `.claude/agents/` (protocol-engineer, rust-implementer, tauri-app-developer, test-writer, build-runner, plan-reviewer, docs-keeper). `docs-keeper` keeps README.md, `app/API.md` and `docs/protocol-g6.md` in sync.
