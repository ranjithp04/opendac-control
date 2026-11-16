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
  - Scout on/off does not change the EQ pre-amp (verified 2026-11-17), so the app keeps the pre-amp usable while Scout is on (Sound Blaster Command disables it).
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

## 3. Known issues and open questions

| # | Item | Plan |
|---|---|---|
| 1 | First reads right after USB plug-in failed (error not captured) | Retry with backoff; log the actual error |
| 2 | Surround showed off after USB replug, came back after headphone jack switch | Re-apply on reconnect implemented. In the M1 replug test Surround was still on afterwards; not reproduced |
| 3 | First write after opening the device returned no ACK (value still applied) | Treat missing ACK as "unknown", rely on read-back |
| 4 | Windows: exclusive access / Sound Blaster Command coexistence | Test in M3 |
| 5 | ~~Feature writes other than EQ enable / pre-amp untested~~ | Done: every exposed feature, SBX/Scout, DAC filter and mic boost write verified by read-back on hardware (2026-11-16) |
| 6 | macOS may require Input Monitoring permission for HID access in a packaged app | Check in M2 |
| 7 | Never write undocumented registers | Feature table is an allow-list |
