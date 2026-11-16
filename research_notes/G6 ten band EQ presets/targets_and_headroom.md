# Acoustic / psychoacoustic foundations for G6 10-band EQ presets (targets, loudness, band roles, headroom, graphic-EQ approximation)

Context: G6 graphic EQ bands 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz, ±12 dB in 0.5 dB steps; separate pre-amp ±6 dB in 0.5 dB steps.

Method note: several numbers below were computed deterministically by me (Python scripts using the ISO 226:2003 formula and RBJ "Audio EQ Cookbook" biquads at fs = 48 kHz). These are labelled "[computed]". They model a generic constant-Q peaking-filter graphic EQ; **the G6's actual filter type/Q is unknown** (see Gaps).

## Harman target curves for over-ear and in-ear headphones

### Takeaway
Relative to a diffuse-field-neutral headphone, the Harman over-ear preference is essentially a low-shelf bass boost of roughly +4 to +6.5 dB below ~105 Hz plus a small (~−1.5 dB) treble trim above ~2.5 kHz; the in-ear target asks for more bass than the over-ear one. A generic preset (which cannot know the user's headphone) can only apply this *delta*, not the absolute eardrum target. About 64% of listeners prefer the Harman target, ~21% want less bass, and a minority want more.

### Cited Findings
- AutoEq "Harman over-ear 2018" target (absolute, eardrum-referenced, normalised 0 dB at 1 kHz), nearest rows: 20 Hz +3.86, 31 Hz +3.94, 62 Hz +2.78, 125 Hz −0.08, 250 Hz −2.02, 500 Hz −0.85, 1 kHz 0.00, 2 kHz +5.29, 3 kHz +8.33, 4 kHz +8.21, 8 kHz +2.44, 10 kHz −0.77, 16 kHz −9.20, 20 kHz −22.92 dB. The +5…+8 dB at 2–4 kHz is the ear-canal/concha "ear gain", which a headphone measured on a head simulator already contains; this is not a boost to apply. — [AutoEq Harman over-ear 2018 CSV](https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/targets/Harman%20over-ear%202018.csv)
- AutoEq "Harman in-ear 2019" target, nearest rows: 20 Hz +7.61, 31 Hz +7.71, 62 Hz +5.62, 125 Hz +1.45, 250 Hz −1.58, 500 Hz −1.41, 1 kHz 0.00, 2 kHz +5.83, 3 kHz +9.32, 4 kHz +8.91, 8 kHz +3.61, 10 kHz −0.87, 16 kHz −10.22, 20 kHz −20.61 dB. — [AutoEq Harman in-ear 2019 CSV](https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/targets/Harman%20in-ear%202019.csv)
- Simple difference of the two CSVs (in-ear minus over-ear) [computed]: 31 Hz +3.8, 62 Hz +2.8, 125 Hz +1.5, 250 Hz +0.4, 2 kHz +0.5, 3 kHz +1.0, 4 kHz +0.7, 8 kHz +1.2 dB. Caveat: the two targets are referenced to different measurement fixtures (over-ear on a head simulator, in-ear on an ear-simulator coupler), so this difference is only indicative. — derived from the two CSVs above
- An AES section lecture summary of the 2019 Harman in-ear work states the new in-ear target has "approximately 10 dB more bass at and below 125 Hz than the around-ear target curve", and that few of 15 commercial IEMs came close to it. This **conflicts** in magnitude with the ~2–4 dB difference implied by the AutoEq CSVs (likely because the 10 dB figure compares different reference conditions; unresolved). — [AES section report](https://aes.org/sections/reports/?ID=3148)
- RTINGS' own (Harman-like) target: diffuse-field response tilted by 6 dB plus a 4.3 dB low-shelf at 105 Hz, Q 0.707. — [RTINGS changelog](https://www.rtings.com/headphones/tests/changelogs/1-8)
- Olive's 2024 pilot "Harman Beta" curve for the B&K 5128: the 5128 diffuse-field correction plus a 6.4 dB low-shelf below 105 Hz and a −1.4 dB high-shelf above 2.5 kHz (Q not stated), adjusted toward the Harman AE 2018 target; 13 listeners; bare diffuse field scored poorly (21.2 vs ~50 for Harman-modified curves), Olive's slide said diffuse field "is not liked despite its status as the current IEC target curve". — [SoundGuys report on CanJam NYC 2024 talk](https://www.soundguys.com/harman-research-targets-the-bruel-kjaer-5128-112247)
- Secondary sources characterise the Harman target as ~5–6 dB more bass than diffuse/free-field targets. — [search summary of Headphonesty / others](https://www.headphonesty.com/2020/04/harman-target-curves-part-3/) (secondary; treat as approximate)
- Listener segmentation (Olive, Welti, Khonsaripour, AES Conv. 146, paper 10156, 2019): Harman target preferred by a majority (64%) across ages, experience and genders; "Less Bass Is Better" class ≈21%, disproportionately female/older, no trained listeners; "More Bass Is Better" class skewed male 4:1; no geographic effect; "slight adjustments in the bass and treble levels may be necessary". — [Acoustics Today, Spring 2022, p.66](https://acousticstoday.org/issues/2022AT/Spring2022/files/basic-html/page66.html); paper listing [AES e-library](https://aes2.org/publications/elibrary-page/?id=20289)
- Secondary coverage gives the "More Bass" group as ~15% preferring more bass below ~300 Hz than Harman. — [Headphonesty 2024](https://www.headphonesty.com/2024/10/harman-target-curve-alive/) (secondary; dB amount not given)
- Shelf values evaluated at the G6 band centres [computed, RBJ low-shelf Q 0.707]:
  - +4.3 dB @105 Hz (RTINGS): 31 +4.3, 62 +3.8, 125 +1.4, 250 +0.1, rest 0.
  - +6.0 dB @105 Hz: 31 +6.0, 62 +5.3, 125 +2.0, 250 +0.2, rest 0.
  - +6.4 dB @105 Hz LS + −1.4 dB @2.5 kHz HS (Olive 2024 beta, Q assumed 0.707): 31 +6.3, 62 +5.7, 125 +2.2, 250 +0.2, 500 0, 1k 0, 2k −0.4, 4k −1.2, 8k −1.4, 16k −1.4.

### Inferences
- For a "Harman-ish" preset on an unknown, roughly diffuse-field-tuned headphone, the apply-able delta is approximately: 31 +5…+6, 62 +4.5…+5.5, 125 +1.5…+2, 250 ~0, 500–2k 0, 4k ≈ −1, 8k/16k ≈ −1.5 dB. (Many consumer headphones already have some bass lift, so this may over-boost on them.)
- "Less bass" subgroup preset: halve the shelf (≈ +2…+3 dB at 31/62). "More bass" preset: +3…+4 dB on top of Harman at 31/62/125, tapering by 250 Hz (since that group is described as wanting more bass below ~300 Hz).
- An in-ear variant could add ≈ +2…+4 dB more at 31/62 and ≈ +1.5 at 125 relative to the over-ear preset (CSV-based; the AES "10 dB" figure is not directly usable).
- Do not boost 2–4 kHz to mimic the absolute Harman ear-gain bump; it is already provided acoustically.

### Gaps
- Harman's original AES papers (2012–2019) are paywalled; exact shelf corner/Q for the 2018 over-ear and 2019 in-ear targets relative to diffuse field were not confirmed from primary text. Values above are read from AutoEq-distributed CSVs and secondary reports.
- Exact dB offsets of the "more/less bass" segments were not found.
- Sean Olive's blog posts were not retrieved directly.

## Equal-loudness contours and loudness / "Night" compensation

### Takeaway
ISO 226 shows that dropping from a reference of ~80 phon to 50–60 phon makes bass sound relatively much weaker; full compensation would need ~+8…+14 dB at 31 Hz and ~+5…+8 dB at 125 Hz relative to 1 kHz, with a small treble rise above ~10 kHz and essentially no change at 2–4 kHz. Practical loudness presets usually apply only part of this.

### Cited Findings
- ISO 226 gives normal equal-loudness-level contours for pure tones, numeric data at 1/3-octave frequencies 20 Hz–12.5 kHz (Annex B). The 2023 revision differs from 2003 mainly at 20 Hz (threshold change of 0.4 dB from ISO 389-7:2019), so 31.5 Hz and above are effectively unchanged. — [Forum Acusticum 2023 paper](https://dael.euracoustics.org/confs/fa2023/data/articles/000585.pdf); [ISO 226:2023 listing](https://iteh.eu/catalog/standards/iso/29732326-85b6-4a3d-817b-88584f147762/iso-226-2023)
- ISO 226:2003 formula implementation (valid 0–90 phon; tables αf, Lu, Tf). — [dsprelated ISO 226 code](https://dsprelated.com/showcode/174.php)
- [computed with the ISO 226:2003 formula] Extra boost needed (relative to 1 kHz) to keep the spectral balance heard at the reference level when listening at a lower level:

| Ref → listen | 31.5 | 63 | 125 | 250 | 500 | 1k | 2k | 4k | 8k | 12.5k |
|---|---|---|---|---|---|---|---|---|---|---|
| 80 → 60 phon | +9.4 | +7.6 | +5.5 | +3.2 | +1.2 | 0 | −0.6 | −0.7 | +0.3 | +3.2 |
| 85 → 60 phon | +11.8 | +9.5 | +6.9 | +4.1 | +1.5 | 0 | −0.8 | −0.9 | +0.3 | +4.1 |
| 80 → 50 phon | +14.0 | +11.2 | +8.1 | +4.7 | +1.7 | 0 | −1.0 | −1.2 | +0.4 | +4.7 |
| 80 → 40 phon | +18.5 | +14.7 | +10.5 | +6.1 | +2.2 | 0 | −1.4 | −1.7 | +0.4 | +6.1 |
| 80 → 30 phon | +22.7 | +17.8 | +12.6 | +7.2 | +2.5 | 0 | −1.8 | −2.3 | +0.3 | +7.1 |

### Inferences
- ISO 226 data stop at 12.5 kHz; the 16 kHz band value must be extrapolated (≈ same as or slightly above 12.5 kHz value) — assumption.
- Full compensation for 80→50 phon (+14 dB at 31 Hz) exceeds the ±12 dB range and the ±6 dB pre-amp headroom; a practical "Night/Loudness" preset would use roughly half to two-thirds compensation, e.g. 31 +6…+8, 62 +5…+6, 125 +3…+4, 250 +1.5…+2, 500 +0.5, 1k–4k 0, 8k 0…+0.5, 16k +2…+3 dB, with matching negative pre-amp. This is a recommendation, not a sourced standard.
- These contours are for pure tones; music loudness perception differs, which is another reason to under-compensate.
- Small desktop speakers often cannot reproduce 31 Hz meaningfully; for a speaker Night preset, shift weight to 62/125.

### Gaps
- No authoritative source found for "typical" commercial loudness-button magnitudes (e.g. Yamaha/Onkyo continuous loudness); values above are derived from ISO 226 only.

## What each octave band does audibly

### Takeaway
Engineering cheat-sheets agree broadly: 31/62 = sub-bass/rumble and kick/bass weight, 62–125 = punch/body, 250 (200–500) = mud/boom, 500 = boxiness/honk onset, 1 kHz = nasal/honk, 2–4 kHz = presence and also harshness, 4–8 kHz = harshness/sibilance, 8 kHz+ = air/sparkle (16 kHz band = top-octave air, low sensitivity for older listeners). Ranges differ between sources, so treat them as zones, not exact points.

### Cited Findings
- iZotope EQ cheat sheet: sub frequencies 20–60 Hz (piano); rumble ~40 Hz (room mics); bass low end 40–120 Hz; kick low-end punch 60–80 Hz; toms fundamental 60–120 Hz; mud 200–500 Hz (piano/strings), 300–500 Hz (bass), 250–600 Hz (toms); boxiness 200–500 Hz (kick), 300–500 Hz (snare), 400–500 Hz (room); honk/boxiness 500–800 Hz (brass); nasal quality 500 Hz–1.2 kHz (vocals); presence or harshness 1–4 kHz (strings, room mics); harshness 2–4 kHz (piano), 2–5 kHz (electric guitar), 3–7 kHz (hi-hats), 5–8 kHz (vocals); air 8–12 kHz, "air and sparkle" 10 kHz+ (vocals). — [iZotope EQ cheat sheet](https://www.izotope.com/en/learn/eq-cheat-sheet)
- Other guides: cut ~250 Hz if muddy; ~500 Hz for honky/veiled; 1 kHz "nasal/honky" on guitars/synths; presence placed variously at ~2 kHz or 3–5 kHz; harshness/sibilance 2–5 kHz or 5–8 kHz; air 8–12 kHz+. — [TeachMeAudio enhancement frequencies](https://www.teachmeaudio.com/mixing/techniques/frequencies/); [Dummies EQ guidelines](https://www.dummies.com/article/technology/software/music-recording-software/general-music-recording-software/home-recording-eq-frequency-guidelines-179779/)

### Inferences
- Mapping to G6 bands: 31 sub-bass/rumble (felt more than heard; mostly headphone-only), 62 kick/bass weight, 125 bass body/punch & warmth, 250 boom/mud, 500 boxiness, 1k nasal/honk and "forwardness", 2k presence/intelligibility, 4k presence-edge/harshness, 8k sibilance/cymbal bite, 16k air.
- Voice/dialog preset levers: cut 250 (−2…−3), small boost 2k/4k (+2…+3), mild 31/62 cut to reduce rumble.
- Note that "2–4 kHz" boosts on headphones push into the region where the ear gain already peaks, so harshness arrives quickly; keep presence boosts modest (≤ +3 dB).

### Gaps
- Sound On Sound / Shure / Neumann frequency guides were not retrieved; descriptors above rest on iZotope plus secondary guides.

## Headroom and pre-amp practice

### Takeaway
Any net boost can push full-scale digital content into clipping, so set pre-amp ≈ −(peak of the combined EQ curve), which with overlapping octave bands can be ~1–3 dB more than the largest single slider. Real masters already have inter-sample peaks of up to ~+2–3 dB over 0 dBFS, so extra margin helps if the G6 DSP has no internal headroom. With the G6's pre-amp limited to −6 dB, presets should keep their combined peak boost ≲ +6 dB.

### Cited Findings
- AutoEq: parametric EQ filters produce positive gains and "to avoid clipping a preamp with negative gain is required"; a few dB more negative preamp may be needed. Rule of thumb: preamp = −(highest filter gain), more if filters overlap; Equalizer APO reports the curve's peak gain, which is the amount to reduce pre-gain. — [AutoEq README (fork)](https://github.com/iwalton3/AutoEq); [Head-Fi Equalizer APO thread](https://www.head-fi.org/threads/equalizer-apo-questions-discussion.953410/post-16135678); [IEM EQ guide](https://iem-eq-guide.pages.dev/how-to-eq/before-getting-started)
- Negative preamp reduces loudness, so users will turn up volume. — [SoundGuys headphone EQ explained](https://www.soundguys.com/headphone-eq-settings-explained-159075/)
- Rane: two adjacent 1/3-octave proportional-Q sliders at +6 dB yield ~+9 dB ("front panel says +6 dB, but the real output is +9 dB"); constant-Q reduces interaction but increases ripple; Rane's constant-Q example of two adjacent +6 dB bands gave ~+7 dB. — [RaneNote 154 Perfect-Q](https://ranecommercial.com/legacy/note154.html); [Rane Constant-Q Graphic Equalizers](https://www.ranecommercial.com/legacy/pdf/ranenotes/Constant_Q_Graphic_Equalizers.pdf)
- [computed, RBJ peaking filters at G6 centres]:
  - Q 1.41: two adjacent +6 → +7.1 dB at the band centres, +6.0 between; three adjacent +6 → +8.3 dB peak; all ten at +6 → 3.7…8.8 dB (ripple, not flat).
  - Q 1.0: two adjacent +6 → +8.1 dB peak; three → +9.8 dB.
  - Q 0.7 (proportional-Q-like wide bands): two adjacent +6 → +9.5 dB peak; three → +11.7 dB.
- Inter-sample peaks: reconstruction can exceed 0 dBFS by up to ~3 dB in theory; BS.1770 true-peak metering uses ≥4× oversampling; streaming services recommend −1 dBTP. — [Essentia true-peak tutorial](https://essentia.upf.edu/tutorial_audioproblems_truepeakdetector.html); [Signalkey true-peak explainer](https://www.signalkey.io/learn/what-is-true-peak)
- Archimago, >107k tracks: headroom needed to cover 99% of tracks' true peaks: Classical +0.79, Soundtracks +1.70, Rock +2.35, Jazz +2.42, Pop +2.87, Rap +3.08, Electronica +4.22 dB; 44% of rock tracks exceed +0.1 dBTP; max seen +7.72 dBTP; recommends ~+3 dB DAC headroom for most music. — [Archimago 2025](https://archimago.blogspot.com/2025/08/a-look-at-music-library-how-much.html?m=1)

### Inferences
- Pre-amp rule for G6 presets: pre-amp = −ceil_to_0.5(peak of summed curve). With Q≈1.4 bands, peak ≈ max slider + ~1 dB when two neighbours are similarly boosted, +2…2.5 dB with three. Because pre-amp bottoms out at −6 dB, a preset whose summed peak exceeds ~+6 dB cannot be fully protected; prefer cutting elsewhere (e.g. express "more bass" partly as mid/treble cuts) so the peak stays ≤ +6 dB.
- A preset with only cuts needs pre-amp 0 (or slightly positive is unnecessary; avoid positive pre-amp because of inter-sample peaks).
- Whether the G6 DSP has internal headroom (floating-point / guard bits before the DAC) is unknown; if it does, clipping risk is lower than modelled.

### Gaps
- No documentation found on the G6's internal DSP word length, headroom, or whether its pre-amp is applied before the EQ.

## Approximating shelf and parametric shapes with octave-band graphic EQ

### Takeaway
A 1-octave graphic EQ (true one-octave bandwidth ⇒ Q ≈ √2 ≈ 1.41) can approximate a low-shelf at ~100 Hz within about ±1–1.5 dB by tapering adjacent bands; keep adjacent steps small (≤ ~2–3 dB) to avoid ripple, and remember neighbours sum (so shelf plateaus need slightly *less* per-band gain than the target level).

### Cited Findings
- Octave graphic EQs use ISO 266 preferred labels 31.5, 63, 125… with ANSI S1.11 exact centres (32, 63, 126, 251…). — [MathWorks graphicEQ](https://au.mathworks.com/help/audio/ref/graphiceq.html)
- A consumer 10-band EQ simulator uses peaking filters at Q 1.4, wider than the octave spacing so bands overlap. — [theaudiostuff EQ simulator](https://theaudiostuff.com/tools/eq-simulator.md)
- Symetrix graphic EQ: "narrow" bands give ~1 dB ripple between two adjacent +6 dB bands, "wide" gives none but more total boost (selectivity vs. combining trade-off). — [Symetrix Graphic EQ docs](https://composer-docs.symetrix.co/software/toolkit/design-view/eqs/Graphic_EQ.html)
- Rane: constant-Q response is nonlinear—boosting a band 4/8/12 dB raises the band 1/3-octave away ~1/2/~3 dB. — [RaneNote 154](https://ranecommercial.com/legacy/note154.html)
- [computed, Q 1.41 model] single +6 dB band: +3.0 dB at ±½ octave, +1.1 dB at ±1 octave.
- [computed, Q 1.41 model, least-squares fit, rounded to 0.5 dB] graphic gains that approximate shelves:
  - LS +4.3 dB @105 Hz Q.707 → 31 +4.0, 62 +3.5, 125 +1.0, 250 −0.5, rest 0 (max err ~1 dB, peak 4.7 dB).
  - LS +6 dB @105 Hz → 31 +5.5, 62 +4.5, 125 +1.5, 250 −0.5 (max err ~1.5 dB incl. band edges, peak 6.4 dB).
  - LS +6.4 @105 + HS −1.4 @2.5k → 31 +6.0, 62 +5.0, 125 +1.5, 250 −0.5, 500–2k 0, 4k −1.0, 8k −1.5, 16k −1.5 (peak 7.0 dB).
  - Naive "6,6,4,2" bass taper overshoots to +7.9 dB at Q 1.41 (+9.2 at Q 1.0), whereas "6,5,3,1" peaks at +7.1.

### Inferences
- Rule of thumb for plateaus: each slider ≈ target − ~1 dB when its neighbours carry similar gain (Q≈1.4); use smaller values if the G6 uses wider (proportional-Q) bands.
- Peak/notch approximations narrower than one octave are not possible; the narrowest feature is one band (≈ ±1 octave skirt).
- The 16 kHz band sits close to Nyquist at 44.1/48 kHz; with bilinear-transform filters its shape is cramped/asymmetric (seen in the model), so its effect above ~16 kHz is uncertain.

### Gaps
- Actual G6 band filter type (peaking biquad? constant/proportional Q?) and Q are undocumented; a measurement (loopback of the G6 output with one band at +12 dB) would be needed to validate summation numbers.
- No normative source found quoting Q 1.41 for ISO octave graphic EQs; 1.41 is the mathematical Q of a one-octave bandwidth (derived, not cited).

## Headphones vs desktop speakers

### Takeaway
The Harman headphone target is designed to mimic the sound of good loudspeakers in a room, so its bass shelf already "adds back" room gain; on speakers the room provides that boost (variably), and the preferred in-room response is a gentle downward tilt. Speaker presets therefore need less low-bass boost (and small desktop speakers cannot use 31 Hz), while headphone presets carry the shelf.

### Cited Findings
- Harman target mimics loudspeakers in a typical listening room, with more bass than diffuse field. — [Headphonesty Harman part 3](https://www.headphonesty.com/2020/04/harman-target-curves-part-3/) (secondary)
- Small rooms: boundaries reinforce bass, absorption reduces treble → downward-sloping in-room response; Toole found listeners prefer loudspeakers with flat, smooth anechoic response and directivity; Olive's room-correction preference was a smooth downward slope, i.e. some natural room gain; real-house bass gains smaller than ideal due to flexible boundaries/openings. — [Archimago room target curves 2026](https://archimago.blogspot.com/2026/07/room-target-curves-then-now-and-options.html); [Olive blog: subjective and objective evaluation of room correction](https://seanolive.blogspot.ca/2009/11/subjective-and-objective-evaluation-of.html); [Harman: Loudspeakers and Rooms Pt 2 (Toole)](https://Harman.com/documents/LoudspeakersandRoomsPt2_0.pdf)
- Preferred tilt magnitude is inconsistently quoted: ~4 dB (±3) over 20 Hz–20 kHz vs ~0.5–1 dB/octave. — [Archimago 2026](https://archimago.blogspot.com/2026/07/room-target-curves-then-now-and-options.html); conflicting forum citations (not primary)
- Equalising a poor speaker to a preferred room curve does not fix it. — [Audioholics forum quoting Toole](https://forums.audioholics.com/forums/goto/post?id=1288162) (secondary)

### Inferences
- Desktop-speaker presets: 31 Hz 0 or negative (avoid excursion/distortion and wasted headroom on small drivers), modest +2…+4 dB at 62/125 if the speaker reaches there; near-field desktop placement has less room gain than across-room listening, so a small lift at 62–125 may be justified; avoid stacking treble boosts (in-room preference tilts down).
- Headphone presets: carry the Harman-style 31/62 shelf (+4…+6 dB) since no room gain exists.
- A "speaker Night" preset should put loudness compensation at 62/125 rather than 31.

### Gaps
- No primary source retrieved quantifying small desktop speaker low-frequency limits (typical −3 dB points ~60–100 Hz are common knowledge but uncited here) or near-field desktop room-gain magnitude.
- Primary Toole/Olive paper text with exact preferred in-room slope not retrieved.
