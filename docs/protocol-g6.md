# Sound BlasterX G6 USB protocol

What OpenDAC Control knows about the Creative Sound BlasterX G6 (USB `041e:3256`) and its HID control protocol.

Everything here was tested on real hardware (G6 firmware `2.1.250903.1324`, macOS, 2026-11-16) unless marked *documented*. *Documented* items come from [linuxblaster_control `UsbProtocol.md`](https://github.com/RizeCrime/linuxblaster_control/blob/master/UsbProtocol.md) (MIT) and were not individually tested.

## 1. Device behaviour

- **Volume knob** sends consumer HID reports on **interface 3**: `01` vol up, `02` vol down, `04` mute (knob press), `00` release. macOS and Windows change system volume natively.
  - When our test tool opened interface 3 with hidapi on macOS, the OS stopped receiving the knob until a USB replug (probably exclusive open). **The app must never open interface 3.**
- **Gain switch**: no USB report at all.
- **SBX button**: turns SBX on; if no effects were remembered, enables Surround. Device pushes `5a 26 … mask` + `5a 11` dump.
- **Scout button**: Scout on ⇒ SBX off, Smart Volume on (0.9), EQ on with Scout curve `2, 3, 3, -2, -2, 0, 3, 4, 3, 2` written into the EQ band registers. Scout off ⇒ device restores previous EQ/SBX state itself.
  - The push on Scout on carries only the mask, Surround off and **EQ off**, with no Scout curve or Smart Volume, even though reading the registers gives EQ on, the curve and Smart Volume 0.9 (verified 2026-11-16). `g6-core` re-reads the Scout-owned features after any Scout change instead of trusting the push. The push on Scout off is a correct full dump.
  - Scout on sets the EQ pre-amp to 0 dB, and Scout off restores the previous value (re-verified 2026-11-17 by raw register reads: -5.5 dB → 0 → -5.5 dB). This contradicts an earlier note that Scout leaves the pre-amp alone, which was probably tested with the pre-amp already at 0. `g6-core` treats the pre-amp like the other Scout-replaced features: re-read after a Scout change (the push does not carry it), left out of profiles and re-applies while Scout is on, never written by Direct Mode. A pre-amp write while Scout is on is accepted and becomes the value kept after Scout turns off (verified 2026-11-17, section 3 item 9), which is why re-applies and profiles must not write it then.
- **Output switch is automatic (headphone jack detection)**, no button. Push `5a 2c 05 01 02` (speakers) / `…04` (headphones), then full dump of the new output's settings. Each output keeps its own EQ/effects.
  - **Mic EQ is per output too** (verified 2026-11-16: headphones mic EQ on with a curve, speakers mic EQ off and flat). Voice Clarity, NR, AEC and recording Smart Volume had the same values on both outputs, so it is not known whether they are per output. `g6-core` stores all mic features (`0x95`) per output; the decoder mode (`0x97`), DAC filter, mic boost and the SBX/Scout mask stay shared.
- **Persistence**: settings survive USB replug (except item 2 in §3).
- macOS sees the G6 via the standard Apple USB audio driver: 44.1/48 kHz, 24/32-bit, 2 ch out, 2 ch in.

## 2. Protocol reference

### 2.1 Transport
- USB `041e:3256`, HID **interface 4**, usage page `0xFF00`, 64-byte input/output, no report ID (send ID 0).
- Frame: `5a CMD LEN PAYLOAD…` zero-padded to 64 bytes. `LEN` = payload bytes after `LEN`.

### 2.2 Commands (verified unless marked)

| Purpose | Send | Reply |
|---|---|---|
| Identity | `5a 05 00` | `5a 05 04 1f 00 00 00` |
| Firmware | `5a 07 01 02` | `5a 07 LEN ascii` → `2.1.250903.1324` (contains a trailing NUL) |
| HW id | `5a 20 00` | `5a 20 04 97 …` |
| DSP version | `5a 30 00` | `5a 30 04 30 01 10 00` |
| Feature read | `5a 11 03 01 FAM ID` | `5a 11 08 01 00 FAM ID f32le` (value at bytes 7..10) |
| Feature write | `5a 12 07 01 FAM ID f32le` | ACK `5a 02 0a 12 STATUS` (`00` ok), then `5a 11` push |
| Global mask read | `5a 26 03 08 ff ff` | `5a 26 0b 08 ff ff MASK` — bit0 SBX, bit1 Scout, bit2 Voice Clarity |
| SBX / Scout write | `5a 26 05 07 [01 SBX \| 02 Scout] 00 [00 off \| 01 on] 00` | |
| Output read | `5a 2c 01 01` | `5a 2c 05 01 [02 speakers \| 04 headphones]` |
| DAC filter read | `5a 6c 01 01` | `5a 6c 03 01 FILTER 00` |
| DAC filter write | `5a 6c 03 00 FILTER 00` | 1 fast-min, 2 slow-min, 3 NOS, 4 fast-lin, 5 slow-lin |
| Mic boost read | `5a 3c 02 01 00` | `5a 3c 04 01 00 02 DB` (0x00/0x0a/0x14/0x1e) |
| Mic boost write | `5a 3c 04 00 00 02 DB` | |

ACK status: `00` ok, `80`/`81`/`83` rejected.

The SBX/Scout, DAC filter and mic boost writes were *documented* only until 2026-11-16; they are now verified by read-back on hardware. Whether the device ACKs them was not checked.

### 2.2a Commands verified on hardware (2026-11-17)

Read-only unless marked. Frames match `linuxblaster_control` `UsbProtocol.md` v5. Used by the code: serial (`g6 info`), output write (`g6 output`, `Device::set_output`), RGB on/off and color (`g6 light`, `set_lighting_*`) and hardware Direct Mode (inside `direct.rs`). Not used: ping, range dump, the enumerate queries and the `5a 3a` capability queries.

| Purpose | Send | Reply |
|---|---|---|
| Serial | `5a 10 00` | `5a 10 08 ef 67 74 00 00 00 00 00` |
| Ping | `5a 06 01 01` | echoed |
| Range dump | `5a 15 01 00` | 3 entries only: `96:0a` ±6 step 0.5, `96:0b` ±12 step 0.5, `96:17` 10–300 step 0.5 |
| Output enumerate | `5a 2c 01 02` | `5a 2c 0a 02 82 02 00 00 00 04 00 00 00` |
| **Output write** | `5a 2c 05 00 [02 speakers \| 04 headphones] 00 00 00` | Works without the jack. Reply: `5a 2c 05 01 MODE`, ACK, then a full profile dump of the new output |
| Mic boost enumerate | `5a 3c 01 02` | `5a 3c 09 02 01 01 00 00 02 00 00 00` |
| DAC filter enumerate | `5a 6c 01 02` | `5a 6c 0e 02 05 85 00 01 00 02 00 03 00 04 00 05 00` |
| **RGB on / off** | `5a 3a 02 06 [01 \| 00]` | ACK `00`; read `5a 3a 01 07` → `5a 3a 02 07 [01 \| 00]` |
| **RGB static color** | mode `5a 3a 06 04 00 03 01 00 01` (ACK `00`; required before the color frame), color `5a 3a 09 0a 00 03 01 01 A B G R` | ACK `00`; read `5a 3a 06 0b 01 00 01 01 01` → `5a 3a 09 0b 01 00 01 01 A B G R` |
| Capability queries | `5a 3a` sub `00`, `05`, `09`, `0e`, `10` | Undecoded: `00`→`01 00 04 00 00`, `05`→`01 00 01 00 01`, `09`→`00 03`, `0e`→`01 01 00 00 00 00`, `10`→`00` |
| **Hardware Direct Mode** | `5a 39 03 00 05 [01 \| 00]`, commit `5a 39 01 01` | ACK `00`, then `5a 39 05 01 20 00 00 04` (on) / `… 00 00 00 04` (off); off also pushes a profile dump. Read `5a 39 01 02` → `5a 39 09 02 20 64 02 04 00 00 00 00` |

- Serial: the 8 payload bytes are the serial number. `g6 info` and the app's "About this device" card read it (`Device::serial`; the app shows it as 16 hex digits); it is never logged. Reply with any other length is not decoded as a serial.
- RGB color bytes are `A B G R` with `A = ff` (red = `ff 00 00 ff`, green = `ff 00 ff 00`, blue = `ff ff 00 00`). Test unit was red and enabled. The LED itself was not observed, only the read-back.
- Hardware Direct Mode changed no DSP register and not the SBX/Scout mask while on, so its effect is on the audio path and is not observable over HID. The app keeps its own software Direct Mode ([`direct.rs`](../crates/g6-core/src/direct.rs)).

### 2.3 Feature table (family / id → f32)

**Family `0x96` — playback**

| ID | Feature | Range |
|---|---|---|
| `00` / `01` | Surround on / level | bool / 0–1 |
| `17` | Surround distance | 10–300, step 0.5 |
| `02` / `03` | Dialog+ on / level | bool / 0–1 |
| `04` / `05` | Smart Volume on / level | bool / 0–1 |
| `06` | Smart Volume mode | 0 normal, 1 loud, 2 night |
| `07` / `08` | Crystalizer on / level | bool / 0–1 |
| `18` / `19` | Bass on / level | bool / 0–1 |
| `09` | EQ on | bool |
| `0a` | EQ pre-amp | ±6, step 0.5 |
| `0b`–`14` | EQ 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz | ±12, step 0.5 |

**Family `0x95` — mic**: `00` AEC, `04` Voice Clarity NR on, `05` NR level (device 0–0.5, UI = ×2), `13` mic EQ on, `14`–`1b` mic EQ bands 1–8 (±12 dB, frequencies unknown), `2c` recording Smart Volume.

*Mic EQ presets (app, partly verified):* the app offers 11 built-in mic EQ presets, written as plain `mic_eq_1`–`mic_eq_8` feature writes (dB, bands 1–8): 1 `-3,-4,0,2,3,-3,4,5`; 2 `-3,-4,0,2,4,-2,2,4`; 3 `-2,-3,3,4,4,-4,3,2`; 4 `-3,-5,0,4,0,-3,0,0`; 5 `-2,-3,2,4,4,0,-3,2`; 6 `-5,-4,-2,0,3,4,6,7`; 7 `0,3,-2,-4,-4,-2,5,7`; 8 `0,0,2,2,3,-4,2,4`; 9 `0,0,2,2,-2,0,-4,4`; 10 `0,2,-2,0,3,5,6,5`; 11 `0,8,0,12,12,4,8,10` (upstream calls it `DM_1`). They were decoded from a `linuxblaster_control` capture of Sound Blaster Command, not measured here. Only preset 1 is confirmed: it equals what this G6 held in its mic EQ bands. The real preset names are unknown (the app calls them `Preset 1`–`Preset 11`), and the other ten have not been checked against the device or by ear.

**Family `0x97`**: `02` decoder mode — 1 Full, 2 Normal, 3 Night. Shown in UI as **Dolby Digital (optical in)** (DRC modes; inferred, not audibly tested).

bool = `0.0` / `1.0`. Values come back with float noise (e.g. `0x34400000` ≈ 0) — round to 0.01 / 0.5 dB.

### 2.4 Push format
`5a 11 LEN COUNT 00` followed by `COUNT` entries of 6 bytes: `FAM ID f32le` (e.g. `5a 11 32 08 00 …` = 8 entries). Dumps also include undocumented ids (`96:70–72`, `95:0a–2d`) — ignore.

Captured examples (use in unit tests):
```
5a 2c 05 01 02                                        # output → speakers
5a 26 0b 08 ff ff 05                                  # mask: SBX + Voice Clarity
5a 11 1a 04 00 96 13 02 00 80 3f 96 14 03 00 00 3f 96 09 00 00 00 00 95 04 00 00 80 3f
5a 11 32 08 00 96 0b fd ff 7f bf 96 0c 00 00 40 34 96 0d 03 00 00 3f 96 0e 02 00 80 3f 96 0f 02 00 80 3f 96 10 02 00 c0 3f 96 11 02 00 80 3f 96 12 02 00 c0 3f
```

### 2.5 State observed on the test unit
- Headphones profile EQ: `0, 0, 0.5, 1, 0, 1.5, 1, 1.5, 1, 0.5` (EQ off).
- Speakers profile EQ: `-1, 0, 0.5, 1, 1, 1.5, 1, 1.5, 1, 0.5`, pre-amp −2.5, Crystalizer on.
- DAC filter fast min-phase, mic boost 30 dB, decoder Normal.

### 2.6 Register sweep (2026-11-17)
Reads of `5a 11 03 01 FAM ID`: family `0x95` answers for ids `00`–`38`, `0x96` for `00`–`41`, `0x97` only `02`. Higher ids are rejected with `5a 02 0a 11 83`.

Unmapped registers that **did not change** when toggling mic Voice Clarity / NR level / AEC / mic EQ, mic boost, Surround (level and distance), Smart Volume (level and mode), Dialog+, DAC filter, Dolby mode, SBX and Scout: `95:01` 0.23, `95:07` 0.05, `95:08`/`09` 30, `95:0b`/`0c`/`0d` 400/1400/2000, `95:0e`/`0f`/`10` 1, `96:15` 0, `96:16` 80 (does not follow Surround Distance), `96:3d` 1, `96:3f` 2; the rest read 0. Treat them as fixed or factory values. Not tested: switching output, which dumps them but they were not diffed.

### 2.7 Mixer: USB Audio Class controls (not HID; read on macOS 2026-11-17)

The G6's playback, monitoring and recording levels are USB Audio Class feature units, so they are not in the HID protocol above. The operating system owns them. On macOS (read-only CoreAudio property queries against the connected G6, nothing written) the audio device is `Sound BlasterX G6`, model UID `Sound BlasterX G6:041E:3256`, transport USB, two channels in each direction. CoreAudio offers:

| Scope | Controls | Range (dB) |
|---|---|---|
| Output (playback) | mute (element 0), volume per channel (elements 1, 2) | -64 to 0 |
| Play-through (monitoring) | mute (element 0), volume per channel | -64 to +9 |
| Input (recording) | mute (element 0), volume per channel, data source selector (`1` Line In, `2` External Mic, `3` S/PDIF In, `4` What U Hear) | -48 to +9 |

All of them were reported settable (`AudioObjectIsPropertySettable`); nothing was set. There is no master volume element (both channels are separate), and the scalar-to-dB curve is not linear (scalar 0.5625 read -16 dB on the output). The same values are also published as control objects (volume, mute, selector classes) and a clock source selector, which the app ignores.

What CoreAudio does **not** show: the per-source monitoring and recording rows of the descriptor (Line In, External Mic, S/PDIF In and What U Hear each with their own level). macOS collapses them to one monitoring level, one recording level for the selected source and the source selector. Whether the monitoring level follows the selected source, and whether the recording level is remembered per source, was not tested because that needs a write. Windows (not verified on a G6): the app uses Core Audio. The G6's render and capture endpoints are found by `vid_041e&pid_3256` in the device id the endpoint's connector is attached to (`IConnector::GetDeviceIdConnectedTo`), else by a friendly name containing "BlasterX G6" or "SBX G6". The endpoint's `IAudioEndpointVolume` gives the master volume (scalar and dB) and mute of playback and recording. The recording source is an `IAudioInputSelector` found by walking the capture endpoint's topology toward the hardware, with the local ids and names of the parts feeding it as options. Windows has no generic play-through control for a USB device, so monitoring is not offered.

The app reads and writes these through `app/src-tauri/src/mixer.rs` (macOS: `mixer/coreaudio.rs`, Windows: `mixer/wasapi.rs`), never through `g6-core`.

## 3. Known issues and open questions

| # | Item | Plan |
|---|---|---|
| 1 | First reads right after USB plug-in failed (error not captured) | Retry with backoff; log the actual error |
| 2 | Surround showed off after USB replug, came back after headphone jack switch | Re-apply on reconnect implemented. In the M1 replug test Surround was still on afterwards; not reproduced |
| 3 | First write after opening the device returned no ACK (value still applied) | Treat missing ACK as "unknown", rely on read-back |
| 4 | Windows: exclusive access / Sound Blaster Command coexistence | Tested 2026-11-17 on Windows (GitHub Actions build): the app connects, controls and live updates work, the volume knob still controls Windows volume, and it reconnects after a USB replug. |
| 5 | ~~Feature writes other than EQ enable / pre-amp untested~~ | Done: every exposed feature, SBX/Scout, DAC filter and mic boost write verified by read-back on hardware (2026-11-16) |
| 6 | macOS may require Input Monitoring permission for HID access in a packaged app | Check in M2 |
| 7 | Never write undocumented registers | Feature table is an allow-list for f32 registers; the other commands (`0x26`, `0x2c`, `0x6c`, `0x3c`, `0x3a`, `0x39`) are built only by typed builders with fixed payloads |
| 8 | Mic EQ band frequencies (`95:14`–`1b`) unknown | Needs an audio measurement, not HID |
| 9 | Scout sets the EQ pre-amp to 0 (see §1); the app shows the device's value and keeps the pre-amp slider active during Scout | Verified 2026-11-17: a pre-amp write during Scout is accepted and the written value stays after Scout turns off (-5.5 → Scout 0 → write -3 → Scout off: -3) |
| 10 | RGB, output write and hardware Direct Mode: read-back/ACK verified (§2.2a). Only static colour is accepted: other values in the mode byte of `5a 3a 06 04 00 MM 01 00 01` are rejected with `83` (`MM` 00–08 tried). Whether the LED visibly changed was not observed | In the app and CLI since 2026-11-17: `g6 light`, `g6 output`, hardware Direct Mode runs inside Direct Mode. Output is a manual override and is not re-applied after a reconnect (jack detection owns it; behaviour of a forced output when the jack changes later is untested) |
