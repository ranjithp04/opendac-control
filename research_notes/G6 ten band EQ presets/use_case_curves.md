# EQ Curves for Listening Use Cases on a 10-Band Graphic EQ (G6: 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz; ±12 dB)

Evidence labels used below: **[EVIDENCE]** = standards body, peer-reviewed or AES work, or manufacturer engineering docs; **[VENDOR]** = a peripheral or software vendor's marketing or blog guidance; **[FOLK]** = community, forum or SEO-blog advice with no test data. Proposed 10-band curves are in the "Inferences" subsections. They are the researcher's synthesis, not sourced presets.

## Footsteps / positional cues in competitive FPS (CS2, Valorant, Apex, PUBG), and comparison with Creative Scout Mode

### Takeaway
Nearly all footstep-EQ guidance is vendor or folk advice. It agrees on a moderate boost (+2 to +4 dB) somewhere in 2–4 kHz, often with the sub-bass and bass reduced to mask fewer explosions and less rumble. No game developer source I found publishes footstep frequency ranges. Creative documents Scout Mode only qualitatively ("boosts quieter sounds like footsteps"), not as an EQ curve.

### Cited Findings
- [VENDOR] SteelSeries says footsteps "usually fall in the mid to high-mid range, around 1 kHz to 4 kHz", and that boosting it "slightly" helps. It gives no dB values. It warns that results "vary by game and headset", that the boosts make other sounds more prominent too, and that cutting bass (20–250 Hz, explosions and engine rumble) reduces impact. — [SteelSeries](https://steelseries.com/blog/best-eq-settings-for-gaming)
- [VENDOR/FOLK] A peripheral vendor's article places footstep transient "definition" at about 2.5–3.5 kHz. It suggests roughly +4 dB at 3 kHz (Q≈1.2) and warns that a broad high-gain boost across all of 2–4 kHz muddies voice chat and makes gunshots harsh. There is no test data. — [Attack Shark](https://attackshark.com/blogs/knowledges/fps-footstep-frequencies-2khz-4khz-mastery)
- [FOLK] Generic gaming-EQ guide: +2 to +4 dB across 2–4 kHz for "step articulation", trim 200–500 Hz for mud, and use smaller boosts on headsets that are already bright. — [EveZone/Evetech](https://evezone.evetech.co.za/deep-dives/eq-settings-footsteps-dialogue)
- [FOLK] A Valorant-specific claim says the game's footsteps are more stylized than CS2's, with a stronger 3–5 kHz "click". — cited in search summaries of [Attack Shark](https://attackshark.com/blogs/knowledges/fps-footstep-frequencies-2khz-4khz-mastery). It is not from Riot and is unverified.
- [FOLK, conflicting] An Elgato guide reportedly lists Valorant footsteps at "100–125 Hz". This contradicts the 2–4 kHz consensus. Both can be partly true, because footsteps are broadband: a low "thud" body plus a high-frequency transient. — [Elgato marketplace](https://marketplace.elgato.com/learn/inspiration/elgato-equalizer-step-up-your-game) (figure not verified on the page)
- [EVIDENCE, developer] Riot (Ask VALORANT #6, audio director Peter Zinda) says the footstep attenuation curve was made "somewhat flat" to prioritise footsteps being *heard* over portraying distance. Riot investigated panning complaints and found the panning correct. Riot gave no frequency information. — [Riot, Ask VALORANT #6](https://riot.com/31zfCBO); [Dot Esports summary](https://dotesports.com/valorant/news/riot-addresses-footstep-sound-complaints-in-valorant)
- [FOLK, conflicting] For CS2, one guide says cut below ~100 Hz and boost ~2 kHz by ~3 dB. Another says CS2's mix has intentional bass that should be kept, and recommends boosting 2–4 kHz instead of cutting heavily. — [Joltfly](https://joltfly.com/best-eq-settings-for-footsteps-dominate-competitive-games); [Switchblade Gaming](https://www.switchbladegaming.com/game-settings/best-gaming-headset/)
- [FOLK] CS2 in-game EQ: some guides recommend the "Crisp" preset (mid emphasis), while another recommends "Performance Mode". They conflict. — [bo3.gg](https://bo3.gg/articles/how-to-make-footsteps-louder-in-cs2); [blog.cs2.ad](https://blog.cs2.ad/best-cs2-audio-settings/)
- [FOLK] A Steam community suggestion is to *cut* the loudest range (~1–7 kHz content such as gunfire) and raise overall volume, rather than boost. This subtractive approach preserves headroom. — [Steam CS2 discussion](https://steamcommunity.com/app/730/discussions/0/3881597531956789677)
- [VENDOR] Creative describes Scout Mode as significantly boosting the audibility of quieter sounds such as footsteps and reloading or weapon-switch sounds. It is a proprietary algorithm, and Creative does not publish its curve or dynamics. — [Creative Scout Mode page](https://sg.creative.com/technology/scoutmode) (fetch returned 403; content via search snippet); [mundobytes explainer](https://mundobytes.com/en/scout-mode-in-creative-app)
- [Review, mixed] Scout Mode's effectiveness is disputed. One reviewer said it felt "almost like cheating" in Call of Duty, another found "no noteworthy benefits" outside FPS, and some users say the whole game gets too quiet. SBX profiles cannot be active at the same time as Scout Mode, and DSP features are unavailable in Direct Mode on some models. — [HotHardware AE-9](https://hothardware.com/reviews/sound-blasterx-ae-9-review?page=4); [TechPowerUp GC7](https://techpowerup.com/review/creative-sound-blaster-gc7/4.html); [r/SoundBlasterOfficial](https://lr.sudovanilla.org/r/SoundBlasterOfficial/comments/17k4rzo/how_useful_is_scout_mode)

### Inferences
- The Scout Mode EQ curve supplied in the assignment (31…16k: +2, +3, +3, −2, −2, 0, +3, +4, +3, +2 dB, plus Smart Volume) is a "V/smile" shape, not a pure footstep curve. It keeps the bass (+2 to +3 dB at 31–125 Hz), scoops 250–500 Hz (−2 dB, a de-mud move), and lifts 2k–8k (+3/+4/+3 dB). This matches the folk "trim 200–500 Hz, +2 to +4 dB at 2–4 kHz" advice. It does *not* follow the "cut sub-bass" school. The quiet-sound lift most likely comes from Smart Volume (compression), not from the EQ. (I could not verify the curve values from a Creative source; they are taken from the assignment brief.)
- Suggested "Footsteps (competitive)" 10-band, for headphones: 31 −4, 62 −3, 125 −1, 250 −1, 500 −1, 1k 0, 2k +2, 4k +4, 8k +2, 16k 0. On a 10-band graphic EQ, the 4 kHz band is the nearest to the 2.5–4 kHz consensus, and 2k adds lower-presence support. Keep 8k modest to limit sibilance and fatigue. Negative low bands also free up headroom (see the small-speakers section).
- Alternative "Footsteps (keep bass)", Scout-like but less treble: 31 +1, 62 +1, 125 0, 250 −2, 500 −2, 1k 0, 2k +2, 4k +3, 8k +2, 16k 0.
- Because footsteps are broadband transients (low body plus high click), cutting the low end works mainly by reducing *masking* from explosions and rumble, not by "revealing" footsteps directly. This is reasoned from the SteelSeries/Elgato descriptions, not measured.
- Trade-offs: boosts of +4 dB or more at 2–8 kHz raise gunfire harshness and sibilance in voice chat, and they increase fatigue in long sessions (see the fatigue section). Prefer net-zero or subtractive shapes, and pre-attenuate if the sum of boosts is large.

### Gaps
- No primary developer data (Valve, Riot, Respawn, Krafton) on footstep spectra was found. All frequency claims are vendor or folk advice.
- No published Creative description of Scout Mode's actual EQ curve or dynamics was found, and the Creative page returned 403. I could not independently confirm the brief's Scout curve values.
- No controlled study was found showing that a footstep EQ improves localization or reaction time. A specpicks claim of a "30–40% warning-time improvement" could not be verified and was excluded.
- No GDC talk on footstep EQ or mix frequency was found within the search budget.

## Speech / podcast / dialogue intelligibility

### Takeaway
The best evidence is ANSI S3.5-1997 (SII). In its octave-band procedure, the 1 kHz, 2 kHz and 4 kHz bands carry about 72% of total band importance, and 2 kHz is the single most important band. Low bands (below 250 Hz) carry little intelligibility but much of the energy. So a playback "dialogue" curve should reduce the lows, de-mud gently, and add a modest lift at 1–4 kHz. Podcast-production EQ advice (HPF at 80 Hz, cut 200–400 Hz, +2–3 dB at 3–5 kHz) points the same way, but it is folk/vendor advice aimed at recording, not playback.

### Cited Findings
- [EVIDENCE] SII is defined as the "product of band importance function and band audibility function, summed over the total number of frequency bands". ANSI S3.5-1997 (R2020) provides critical-band (21), one-third-octave (18), equally-contributing and octave-band (6) procedures. It replaced the Articulation Index (first issued 1969). — [ANSI blog](https://blog.ansi.org/speech-intelligibility-index); [MOSQITO docs](https://mosqito.readthedocs.io/en/stable/source/reference/mosqito.sq_metrics.speech_intelligibility.sii_ansi.sii_ansi.html)
- [EVIDENCE] Octave-band importance weights, as reproduced in the R SII package (Table 4): 250 Hz ≈ 0.0617, 500 Hz ≈ 0.1671, 1 kHz ≈ 0.2373, 2 kHz ≈ 0.2648, 4 kHz ≈ 0.2142, 8 kHz ≈ 0.0549. (The values come from the package's example; I did not verify them against the official standard text.) — [R SII package docs](https://rdrr.io/cran/SII/man/sii.html); [SII vignette](https://rdrr.io/cran/SII/f/inst/doc/SII.pdf)
- [EVIDENCE] Haggard (Institute of Acoustics paper) puts the speech *energy* centroid around 700–800 Hz and the *intelligibility* centroid near 1.8 kHz. The 1.8–6 kHz region conveys as much intelligibility as the lower portion, on less energy. — [IOA / Haggard PDF](https://www.ioa.org.uk/system/files/publications/MP%20HAGGARD%20SPEECH%20FOR%20HEARING%20OR%20HEARING%20FOR%20SPEECH%20IMPLICATIONS%20FOR%20THEORY%20AND%20PERCEPTUAL%20TESTING.pdf)
- [VENDOR, acoustics] Vowel energy sits mainly at about 250–2,000 Hz, voiced consonants at 250–4,000 Hz, and unvoiced consonants at 2,000–8,000 Hz. — [Ecophon](https://ecophon.com/en/knowledge/acoustic-knowledge/basic-acoustics/generating-and-understanding-speech)
- [EVIDENCE, caveat] A clinical linguistics article warns that it is "erroneous" to say vowels are mostly low and consonants mostly high, because nasals and liquids are low-frequency. — [Hearing Review](https://hearingreview.com/practice-building/practice-management/continuing-education/back-basics-linguistics-101-hearing-healthcare-professionals)
- [FOLK] The often-repeated "63–500 Hz = 60% of power and 5% of intelligibility; 1–8 kHz = 5% of power and 60% of intelligibility" has no primary source I could find. — [homerecording.com forum](https://homerecording.com/bbs/threads/unpleasant-high-end-in-vocals.166606/page-2)
- [FOLK/VENDOR, production] Podcast voice EQ consensus: high-pass at 80–100 Hz (12–18 dB/oct), a gentle cut at 200–400 Hz for mud or boxiness, +2 to +3 dB at 3–5 kHz for presence, and de-essing at about 5–10 kHz *before* the presence boost. Make small moves and compare against bypass. — [Cleanvoice](https://cleanvoice.ai/blog/podcast-eq/); [StreamGeeks](https://streamgeeks.us/how-to-eq-a-podcast/amp); [Music Guy Mixing](https://www.musicguymixing.com/voice-eq/); [RØDE processing guide](https://rode.com/pt-br/about/news-info/a-guide-to-audio-processing-and-fx-for-podcasting)

### Inferences
- Suggested "Dialogue / Podcast" 10-band: 31 −6, 62 −4, 125 −2, 250 −2, 500 −1, 1k +1, 2k +3, 4k +2, 8k 0, 16k −1. The weighting follows the SII ranking (2k > 1k ≈ 4k > 500 > 250 ≈ 8k). The 31/62 cuts emulate a high-pass, since the G6 graphic EQ has no HPF. The 250 cut is the de-mud move. 8k stays flat to avoid sibilance.
- Playback EQ cannot raise the SII if the speech is already fully audible. Its value lies in reducing low-frequency masking (upward spread of masking from bass and music beds) and in compensating for headphones or speakers with recessed upper mids. This is reasoned from SII's audibility-times-importance definition.

### Gaps
- No playback-side dialogue EQ standard was found from EBU, NPR or Shure within the search budget. The podcast advice is all production-side and blog-sourced.
- The official ANSI S3.5 Table 4 text was not accessed directly.

## Movies / immersive gaming (cinematic bass), dialogue clarity, and night or dynamic-range concerns

### Takeaway
For immersion, the common advice is a low-shelf lift (31–125 Hz) plus some upper-treble "air". There is no standard "cinematic" curve, and the evidence base is listener-preference research rather than film standards. For night or low-volume listening, the evidence-based tools are dynamic-range compression (Dolby DRC "Line/RF" modes, AVR "Night Mode") and loudness compensation based on equal-loudness contours (ISO 226), not a static EQ alone.

### Cited Findings
- [EVIDENCE] Harman headphone research: in Olive's AES 2024 in-ear study (B&K 5128), Harman 2019 and SoundGuys curves were preferred by 72% of listeners. A second, less-trained and older class (28%) preferred *less bass and more treble*, but SoundGuys' summary says untrained listeners preferred *more* of both. The sources conflict, so check the paper. — [SoundGuys](https://soundguys.com/harman-target-soundguys-preference-curve-validated-125420); [Headphonesty](https://www.headphonesty.com/2024/10/harman-target-curve-alive/); [AES e-library 22696](https://aes2.org/publications/elibrary-page/?id=22696); [SoundGuys on 5128](https://www.soundguys.com/harman-research-targets-the-bruel-kjaer-5128-112247)
- [EVIDENCE] Olive's 2025 method-of-adjustment study: preferred bass ranged from about +2 to +9 dB and treble from about −5 to +2 dB relative to diffuse field. Adjustments of about 3 dB accommodate most individual taste. — [AES e-library 22928](https://aes2.org/publications/elibrary-page/?id=22928)
- [EVIDENCE] Olive suggests the *ratio* of bass to treble may matter more than absolute levels, and calls the target a guideline, not a rule. — [SoundGuys](https://soundguys.com/harman-target-soundguys-preference-curve-validated-125420)
- [VENDOR] Listeners in background noise reportedly prefer more bass and less treble. — [Wevolver/USound](https://www.wevolver.com/article/how-earphones-should-sound)
- [VENDOR] SteelSeries: bass (20–250 Hz) carries explosions and engine rumble, so cutting it for clarity costs impact. — [SteelSeries](https://steelseries.com/blog/best-eq-settings-for-gaming)
- [EVIDENCE, Dolby docs] Dolby Digital decoders offer "Line" mode (dialnorm reference −31 dBFS, DRC with user high/low scaling) and "RF" mode (−20 dBFS, maximum dynamic-range reduction, no scaling). — [Microsoft AC-3 decoder API](https://learn.microsoft.com/en-us/windows/win32/api/codecapi/ne-codecapi-eavdecddoperationalmode); [Dolby DP570 manual, DRC](https://www.manualsdir.com/manuals/73439/dolby-laboratories-dp570.html?page=100); [Dolby DP569 manual](https://www.manualsdir.com/manuals/76738/dolby-laboratories-dp569.html?page=65)
- [EVIDENCE, manufacturer] Dialnorm sets a common dialogue reference level, and DRC compresses around it. AVR "Night Mode" reduces the gap between loud and soft sounds; Denon's version works only on Dolby Digital sources. — [Hybrik Dolby audio](https://docs.hybrik.com/tutorials/dolby_audio/); [Denon manual Night Mode](https://manuals.denon.com/avrs500bt/na/en/WBSPSYtkodivyx.php)

### Inferences
- Suggested "Cinematic / Immersive" 10-band: 31 +5, 62 +4, 125 +2, 250 0, 500 −1, 1k 0, 2k +1, 4k +1, 8k +2, 16k +2. This is a bass shelf within Olive's preferred +2 to +9 dB band-of-taste, with a small presence lift so dialogue survives the bass. Pre-attenuate about 5 dB to avoid clipping.
- Combine it with the G6's own dynamics processing, such as Smart Volume or SBX Dialog Plus where available, for dialogue in loud mixes. EQ cannot fix the dynamic-range problem; compression can.

### Gaps
- No film or broadcast standard defines a consumer "cinematic" playback EQ. I did not research cinema X-curve material, as it applies to large rooms, not headphones or desktops.
- Whether the G6 applies Dolby DRC to decoded streams is not known from these sources.

## Night / low-volume listening

### Takeaway
At low SPL, hearing loses sensitivity to bass (and to a lesser extent the extreme treble) faster than to midrange (ISO 226 equal-loudness contours). Classic "loudness" compensation therefore boosts lows and highs at low volume. For night listening, pair it with compression so peaks don't wake others.

### Cited Findings
- [EVIDENCE] ISO 226 (derived from Fletcher–Munson 1933 and Robinson–Dadson 1956; revised 2003 and 2023) shows lower sensitivity to low frequencies at low levels. Perceived bass falls disproportionately as playback level drops. — [Wikipedia: Equal-loudness contour](https://en.wikipedia.com/wiki/Equal-loudness_contour); [cmtext acoustics primer](https://cmtext.com/acoustics/chapter1_loudness.php)
- [EVIDENCE] Loudness compensation boosts the low and high frequencies to offset this fall-off at low volume. Some designs scale the boost with the volume setting. — [Extron: The Mysterious Loudness Control](https://azwebdev.extron.com/article/loudnesscontrol_ts?tab=tools); [diyAudio thread](https://www.diyaudio.com/community/threads/equal-loudness-curves-fletcher-munson-iso-226-etc-why-do-we-care.139187)
- [Caveat] The contours are statistical averages measured with headphones or in anechoic conditions, and they vary in real rooms. — [Wikipedia: Equal-loudness contour](https://en.wikipedia.com/wiki/Equal-loudness_contour)
- [VENDOR] Commercial ISO 226-based loudness features exist, for example Luxsin, calibrated at 80 phon. — [Luxsin](https://luxsinaudio.com/blogs/luxsin/understanding-luxsin-loudness-feature-smarter-listening-at-any-volume)
- [EVIDENCE] Night mode, or DRC, reduces loud/soft differences (see the Dolby/Denon sources in the movies section). — [Denon manual](https://manuals.denon.com/avrs500bt/na/en/WBSPSYtkodivyx.php)

### Inferences
- Suggested "Night / Low volume" 10-band: 31 +4, 62 +4, 125 +2, 250 0, 500 0, 1k 0, 2k +1, 4k 0, 8k +1, 16k +2. This is a mild loudness contour. On *speakers* at night, reduce 31/62 instead, because low bass travels through walls. A speaker-night variant is: 31 −6, 62 −3, 125 0, 250 0, 500 0, 1k +1, 2k +2, 4k +1, 8k 0, 16k 0, combined with Smart Volume or compression.
- The ideal loudness compensation depends on playback level, so a single static preset is a compromise.

### Gaps
- No measured dB values for how much bass boost is needed at, for example, 50 vs 80 phon in the 31–125 Hz bands were extracted from the ISO tables, because the standard text was not accessed.

## Small / laptop-class desktop speakers

### Takeaway
Small drivers have limited excursion. Boosting below their roll-off (often about 80–200 Hz for laptops, about 45–60 Hz F3 for decent 4-inch desktop monitors) mostly adds distortion and over-excursion risk and consumes digital headroom. A good "small speakers" curve *cuts* below the roll-off and adds a modest lift just above it (about 100–250 Hz) plus a presence lift.

### Cited Findings
- [EVIDENCE, patent] Boosting content below a speaker's lower cutoff frequency can drive the cone beyond its normal mechanical range and add nonlinear distortion, which is why the patented scheme limits the boosted band. — [US Patent 9344822](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/9344822)
- [EVIDENCE, white paper] Excursion-limited SPL drops severely below the port tuning frequency (fb), which makes EQ of ported systems below tuning usually impractical. — [Cordell EQSS white paper](https://www.cordellaudio.com/loudspeakers/EQSS_White_Paper.pdf)
- [FOLK] "Excursion = distortion"; ported boxes need a high-pass below tuning to avoid over-excursion; and EQ-only bass boost needs power and headroom that small wideband drivers rarely have. — [diyAudio post](https://www.diyaudio.com/community/goto/post?id=4007174); [diyAudio post](https://www.diyaudio.com/community/goto/post?id=3351851)
- [FOLK] Rule of thumb: each +3 dB of boost doubles the amplifier power needed in that band, so lower the EQ output gain by the amount of the boost. — [bestsounds.net](https://bestsounds.net/?p=32628)
- [FOLK/measurement anecdote] Laptop speaker cutoffs vary widely. A bass-test site says about 150–200 Hz, another says 80–100 Hz, and a user measured a 15" MacBook Pro rolling off at 120–130 Hz, with 110 Hz "practically inaudible". — [mic-tests bass test](https://mic-tests.github.io/bass-test); [Framework community](https://community.frame.work/t/speakers-sound-quality/1078/34)
- [EVIDENCE, measurements] Desktop monitors measured with CTA-2034 / near-field: Topping MA4 has useful extension to about 45 Hz, and Edifier MR4.5 has an F3 of about 60 Hz and an F10 of about 44 Hz. Computer speaker "20 Hz–20 kHz" specs are often rated at −10 dB or worse. — [Erin's Audio Corner MA4](https://erinsaudiocorner.com/loudspeakers/topping_ma4/); [Erin's Audio Corner MR4.5](https://erinsaudiocorner.com/loudspeakers/edifier_mr4_5/); [O'Reilly PC Hardware](https://www.safaribooksonline.com/library/view/pc-hardware-in/059600513X/ch18s01.html)

### Inferences
- Suggested "Small / laptop speakers" 10-band: 31 −12, 62 −6, 125 +2, 250 +2, 500 0, 1k 0, 2k +1, 4k +1, 8k 0, 16k −1. Cutting 31/62 removes energy that these speakers cannot reproduce, which reduces distortion, port chuffing and DSP-limiter pumping. The +2 at 125/250 adds perceived warmth where the drivers still have output.
- For about 4-inch desktop monitors with F3 around 60 Hz, use a milder version: 31 −6, 62 −1, 125 +1, 250 0, everything else flat.
- On a fixed-point DAC EQ, any net positive boost needs equal pre-attenuation (or volume headroom) to avoid digital clipping. Whether the G6 applies automatic pre-gain is not known from these sources.

### Gaps
- No Creative documentation was found on whether the G6 EQ has a pre-gain or limiter.
- Laptop cutoff figures are anecdotal, and no systematic measurement survey was found.

## Reducing listening fatigue (warm / treble-reduced curves)

### Takeaway
Evidence supports a gentle *downward tilt* as generally preferred. Harman in-room work suggests about −0.5 to −1 dB/octave, or about −6 dB from 100 Hz to 10 kHz. Harshness usually comes from peaks at about 5–9 kHz, from sealed-ear resonances and sibilance, so fatigue curves should use broad, modest cuts there rather than deep notches on a graphic EQ.

### Cited Findings
- [EVIDENCE] Olive (2009, AES): preferred room corrections had "a smooth downward slope with increasing frequency". — [Sean Olive blog](https://seanolive.blogspot.ca/2009/11/subjective-and-objective-evaluation-of.html)
- [EVIDENCE, secondary] Harman listening tests reportedly preferred about −6 dB from 100 Hz to 10 kHz (about −0.9 dB/oct), with an in-room target of about −0.5 dB/oct. — [Saunisto PDF via Midwest Audio Club](https://discourse.midwestaudio.club/uploads/short-url/n3Hy1Mh1ftXlY1UXnfsvdSHGgWg.pdf); Toole's small-room curve shows about 8 dB of slope from bass to treble — [Archimago 2026](https://archimago.blogspot.com/2026/07/room-target-curves-then-now-and-options.html?m=1)
- [EVIDENCE, patent/engineering] Sealed-ear (insert) resonances fall above 5 kHz, with first resonances clustering at about 5–9 kHz. USound describes a new dominant resonance near 8 kHz that depends on ear geometry and insertion depth. — [US Patent 9282397](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/9282397); [USound](https://usound.com/?p=11046)
- [VENDOR glossary] Sibilance is harsh "s/sh/ts" from exaggerated energy at about 5–10 kHz, and fricative energy sits at about 4–10 kHz. — [theaudiostuff glossary](https://theaudiostuff.com/glossary/sibilance/)
- [FOLK/review] Narrow treble peaks differ between ears and between listeners, and aggressive EQ at 8 kHz "often causes more problems than it solves". Broad, cautious compensation works better. — [headphones.com Ananda review](https://headphones.com/blogs/reviews/hifiman-ananda-unveiled-review-one-big-problem); [USound](https://usound.com/?p=11046)
- [EVIDENCE] Individual preference varies by about ±3 dB in bass and treble (Olive 2025), so a fatigue preset should be gentle. — [AES e-library 22928](https://aes2.org/publications/elibrary-page/?id=22928)
- [General] Listener fatigue has multiple causes, including level and duration, not only spectrum. — [Hearing Health Matters](https://hearinghealthmatters.org/hearingnewswatch/2011/causes-and-cures-for-listener-fatigue/)

### Inferences
- Suggested "Warm / Low-fatigue" 10-band, a tilt of about −0.7 dB/oct: 31 +2, 62 +2, 125 +1, 250 +1, 500 0, 1k 0, 2k −1, 4k −2, 8k −3, 16k −3. The broad 4k/8k cuts address the 3–9 kHz harshness region without narrow notches, which a 10-band graphic EQ cannot make anyway.
- Footstep and dialogue presets inherently push 2–8 kHz up, which conflicts with fatigue reduction. For long sessions, cap high-mid boosts at about +3 dB and avoid boosting 8k and 16k.

### Gaps
- No controlled study was found that links a specific EQ shape to reduced *fatigue*, as opposed to *preference*. The tilt evidence is preference data.
- The G6 band shapes (Q, filter type) were not documented in these sources, and adjacent-band interaction affects the actual curve.
