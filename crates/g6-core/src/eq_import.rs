//! Import an EQ from AutoEq / Equalizer APO text onto the G6's 10 bands and
//! pre-amp. Pure functions: no device, no files.
//!
//! Understood lines (case-insensitive, `#` starts a comment):
//!
//! ```text
//! Preamp: -5.2 dB
//! Filter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.0
//! Filter 2: ON LSC Fc 105 Hz Gain 5.3 dB Q 0.70
//! GraphicEQ: 31 -3; 62 1.5; 125 0
//! ```
//!
//! Filter types `PK`/`PEQ` (peaking) and `LS`/`LSC`/`HS`/`HSC` (shelves) are
//! evaluated with the RBJ biquad formulas: the summed response of all `ON`
//! filters, in dB, is read at the 10 band centres. `OFF` filters are skipped;
//! other types (low / high / band pass, notch, all-pass) cannot be shown on 10
//! gain bands and are skipped with a note. `GraphicEQ` points are interpolated
//! linearly over log frequency (flat beyond the first / last point). The
//! result is rounded to 0.5 dB and clamped to +-12 dB (pre-amp +-6 dB).
//!
//! ASSUMPTIONS: a 48 kHz sample rate for the biquads, the nominal band
//! centres in [`BAND_HZ`], and a shelf without a Q uses 0.707. The G6's own
//! band filter shapes are not documented, so the result is an approximation;
//! [`EqImport::notes`] says what was approximated or clamped.

use std::f64::consts::PI;
use std::fmt;

use crate::features;

/// Largest text accepted (real AutoEq files are a few KiB).
pub const MAX_INPUT_BYTES: usize = 1 << 20;

/// Sample rate the biquads are evaluated at.
pub const SAMPLE_RATE: f64 = 48_000.0;

/// Band centres in Hz, in band order (`features::EQ_BANDS`).
pub const BAND_HZ: [f64; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

const BAND_LABELS: [&str; 10] = [
    "31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz",
];

const BAND_LIMIT: f64 = 12.0;
const PREAMP_LIMIT: f64 = 6.0;
/// Shelf Q when the line has none (Butterworth, the Equalizer APO `LS` / `HS` slope).
const DEFAULT_SHELF_Q: f64 = 0.707;
/// Sanity limit for one filter's gain; keeps the maths finite.
const MAX_FILTER_GAIN_DB: f64 = 100.0;

/// The mapped EQ: UI values (dB), ready for `apply_eq_curve`.
#[derive(Debug, Clone, PartialEq)]
pub struct EqImport {
    /// 31 Hz .. 16 kHz, multiples of 0.5 within +-12.
    pub bands: [f32; 10],
    /// Multiple of 0.5 within +-6.
    pub preamp: f32,
    /// What was approximated, clamped or ignored, for display.
    pub notes: Vec<String>,
}

/// Why a text could not be imported.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    TooLarge,
    /// A recognised line that cannot be used (1-based line number).
    Line {
        line: usize,
        message: String,
    },
    /// No `Preamp`, `Filter` or `GraphicEQ` line at all.
    NoEqData,
    /// The mapped values failed the feature table (a bug, not user input).
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge => write!(f, "EQ text is larger than {} KiB", MAX_INPUT_BYTES / 1024),
            Error::Line { line, message } => write!(f, "line {line}: {message}"),
            Error::NoEqData => f.write_str(
                "no EQ found: expected 'Preamp:', 'Filter N: ON PK Fc .. Gain .. Q ..' or 'GraphicEQ:' lines (AutoEq / Equalizer APO)",
            ),
            Error::Invalid(m) => write!(f, "invalid EQ: {m}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy)]
struct Filter {
    shape: Shape,
    fc: f64,
    gain_db: f64,
    q: f64,
}

/// Biquad coefficients, `a0` included.
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
}

impl Filter {
    /// RBJ audio EQ cookbook coefficients (peaking EQ, low / high shelf with Q).
    fn biquad(&self) -> Biquad {
        let a = 10f64.powf(self.gain_db / 40.0);
        let w0 = 2.0 * PI * self.fc / SAMPLE_RATE;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * self.q);
        let sa = 2.0 * a.sqrt() * alpha;
        match self.shape {
            Shape::Peak => Biquad {
                b: [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
                a: [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
            },
            Shape::LowShelf => Biquad {
                b: [
                    a * ((a + 1.0) - (a - 1.0) * cos + sa),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - sa),
                ],
                a: [
                    (a + 1.0) + (a - 1.0) * cos + sa,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - sa,
                ],
            },
            Shape::HighShelf => Biquad {
                b: [
                    a * ((a + 1.0) + (a - 1.0) * cos + sa),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - sa),
                ],
                a: [
                    (a + 1.0) - (a - 1.0) * cos + sa,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - sa,
                ],
            },
        }
    }

    /// Magnitude response in dB at `hz`.
    fn response_db(&self, hz: f64) -> f64 {
        let Biquad { b, a } = self.biquad();
        let w = 2.0 * PI * hz / SAMPLE_RATE;
        // |c0 + c1 e^-jw + c2 e^-2jw|^2
        let mag2 = |c: &[f64; 3]| {
            let re = c[0] + c[1] * w.cos() + c[2] * (2.0 * w).cos();
            let im = -(c[1] * w.sin() + c[2] * (2.0 * w).sin());
            re * re + im * im
        };
        10.0 * (mag2(&b) / mag2(&a)).log10()
    }
}

/// A number with an optional unit suffix ("-3.0", "1000Hz", "-5.2dB"). Text
/// copied from a web page may use the Unicode minus (U+2212) for '-'.
fn number(tok: &str, unit: &str) -> Option<f64> {
    let t = tok.trim_end_matches(',');
    // `get` is None when the cut is not on a char boundary (non-ASCII tail).
    let t = match t.len().checked_sub(unit.len()).and_then(|i| t.get(i..)) {
        Some(tail) if tail.eq_ignore_ascii_case(unit) => &t[..t.len() - unit.len()],
        _ => t,
    };
    t.replace('\u{2212}', "-")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(40).collect();
    if s.chars().count() > 40 {
        out.push_str("...");
    }
    out
}

#[derive(Default)]
struct Parsed {
    preamp_db: f64,
    filters: Vec<Filter>,
    graphic: Vec<(f64, f64)>,
    /// Any Preamp / Filter / GraphicEQ line (even an OFF filter) was seen.
    saw_eq: bool,
    notes: Vec<String>,
}

fn line_err<T>(line: usize, message: impl Into<String>) -> Result<T, Error> {
    Err(Error::Line {
        line,
        message: message.into(),
    })
}

impl Parsed {
    fn parse_line(&mut self, line: usize, raw: &str) -> Result<(), Error> {
        let text = raw.split('#').next().unwrap_or("").trim();
        if text.is_empty() {
            return Ok(());
        }
        let Some((head, rest)) = text.split_once(':') else {
            self.notes
                .push(format!("line {line} ignored: '{}'", short(text)));
            return Ok(());
        };
        let head = head.trim().to_ascii_lowercase();
        let name = head.split_whitespace().next().unwrap_or("");
        match name {
            "preamp" => self.preamp(line, rest),
            "filter" => self.filter(line, rest),
            "graphiceq" => self.graphic_eq(line, rest),
            _ => {
                self.notes
                    .push(format!("line {line} ignored: '{}'", short(text)));
                Ok(())
            }
        }
    }

    fn preamp(&mut self, line: usize, rest: &str) -> Result<(), Error> {
        let tok = rest.split_whitespace().next().unwrap_or("");
        let Some(db) = number(tok, "db") else {
            return line_err(line, format!("pre-amp '{}' is not a number of dB", tok));
        };
        self.saw_eq = true;
        // Equalizer APO adds up several Preamp lines.
        self.preamp_db += db;
        Ok(())
    }

    fn filter(&mut self, line: usize, rest: &str) -> Result<(), Error> {
        let toks: Vec<&str> = rest.split_whitespace().collect();
        let on = match toks.first().map(|t| t.to_ascii_lowercase()).as_deref() {
            Some("on") => true,
            Some("off") => false,
            _ => return line_err(line, "filter must start with ON or OFF"),
        };
        self.saw_eq = true;
        if !on {
            return Ok(());
        }
        let Some(kind) = toks.get(1).map(|t| t.to_ascii_lowercase()) else {
            return line_err(line, "filter has no type (PK, LSC, HSC, ...)");
        };
        let shape = match kind.as_str() {
            "pk" | "peq" => Shape::Peak,
            "ls" | "lsc" | "lsq" => Shape::LowShelf,
            "hs" | "hsc" | "hsq" => Shape::HighShelf,
            other => {
                self.notes.push(format!(
                    "line {line}: filter type {} cannot be shown on gain bands, ignored",
                    other.to_ascii_uppercase()
                ));
                return Ok(());
            }
        };
        let (mut fc, mut gain, mut q) = (None, None, None);
        let mut i = 2;
        while i < toks.len() {
            let key = toks[i].to_ascii_lowercase();
            let value = toks.get(i + 1).copied();
            match (key.as_str(), value) {
                ("fc", Some(v)) => fc = Some(number(v, "hz")),
                ("gain", Some(v)) => gain = Some(number(v, "db")),
                ("q", Some(v)) => q = Some(number(v, "")),
                ("bw", _) => return line_err(line, "BW is not supported, give the filter a Q"),
                _ => {
                    i += 1;
                    continue;
                }
            }
            i += 2;
        }
        let fc = match fc {
            Some(Some(v)) if v > 0.0 && v < SAMPLE_RATE / 2.0 => v,
            Some(_) => return line_err(line, "Fc must be a frequency in Hz below 24000"),
            None => return line_err(line, "filter has no Fc"),
        };
        let gain_db = match gain {
            Some(Some(v)) if v.abs() <= MAX_FILTER_GAIN_DB => v,
            Some(_) => {
                return line_err(
                    line,
                    format!("Gain must be a number of dB within +-{MAX_FILTER_GAIN_DB}"),
                )
            }
            None => return line_err(line, "filter has no Gain"),
        };
        let q = match (q, shape) {
            (Some(Some(v)), _) if v > 0.0 => v,
            (Some(_), _) => return line_err(line, "Q must be a positive number"),
            (None, Shape::Peak) => return line_err(line, "peaking filter has no Q"),
            (None, _) => {
                self.notes.push(format!(
                    "line {line}: shelf without Q, using {DEFAULT_SHELF_Q}"
                ));
                DEFAULT_SHELF_Q
            }
        };
        self.filters.push(Filter {
            shape,
            fc,
            gain_db,
            q,
        });
        Ok(())
    }

    fn graphic_eq(&mut self, line: usize, rest: &str) -> Result<(), Error> {
        let before = self.graphic.len();
        for pair in rest.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let toks: Vec<&str> = pair.split_whitespace().collect();
            let (Some(hz), Some(db)) = (
                toks.first().and_then(|t| number(t, "hz")),
                toks.get(1).and_then(|t| number(t, "db")),
            ) else {
                return line_err(
                    line,
                    format!("GraphicEQ point '{}': expected 'HZ DB'", short(pair)),
                );
            };
            if toks.len() != 2 || hz <= 0.0 {
                return line_err(
                    line,
                    format!("GraphicEQ point '{}': expected 'HZ DB'", short(pair)),
                );
            }
            self.graphic.push((hz, db));
        }
        if self.graphic.len() == before {
            return line_err(line, "GraphicEQ has no points");
        }
        self.saw_eq = true;
        Ok(())
    }

    /// Interpolated GraphicEQ gain at `hz`; the points are sorted by frequency.
    fn graphic_at(&self, hz: f64) -> f64 {
        let g = &self.graphic;
        let (Some(first), Some(last)) = (g.first(), g.last()) else {
            return 0.0;
        };
        if hz <= first.0 {
            return first.1;
        }
        if hz >= last.0 {
            return last.1;
        }
        let i = g.partition_point(|p| p.0 <= hz);
        let ((f0, g0), (f1, g1)) = (g[i - 1], g[i]);
        if f1 <= f0 {
            return g0;
        }
        g0 + (g1 - g0) * (hz.ln() - f0.ln()) / (f1.ln() - f0.ln())
    }
}

/// Round to 0.5 dB; no "-0".
fn round_half(v: f64) -> f64 {
    (v * 2.0).round() / 2.0 + 0.0
}

/// Round to 0.5 dB, then clamp to `+-limit`; a note when the clamp changed it.
fn fit(label: &str, value: f64, limit: f64, notes: &mut Vec<String>) -> f32 {
    let r = round_half(value);
    let c = r.clamp(-limit, limit);
    if c != r {
        notes.push(format!(
            "{label}: {value:+.1} dB clamped to {c:+.1} dB (the G6 allows +-{limit} dB)"
        ));
    }
    c as f32
}

/// Map AutoEq / Equalizer APO `text` to the 10 bands and pre-amp.
pub fn parse_eq_text(text: &str) -> Result<EqImport, Error> {
    if text.len() > MAX_INPUT_BYTES {
        return Err(Error::TooLarge);
    }
    // Windows editors add a byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = Parsed::default();
    for (i, raw) in text.lines().enumerate() {
        p.parse_line(i + 1, raw)?;
    }
    if !p.saw_eq {
        return Err(Error::NoEqData);
    }
    p.graphic.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut notes = Vec::new();
    if !p.filters.is_empty() {
        notes.push(format!(
            "Filters were evaluated at the 10 band centres (RBJ biquads at {} kHz); the G6's own band shapes are not documented, so this is an approximation.",
            SAMPLE_RATE / 1000.0
        ));
    }
    if !p.graphic.is_empty() {
        notes.push(
            "GraphicEQ points were interpolated over log frequency at the 10 band centres."
                .to_string(),
        );
    }
    let mut bands = [0f32; 10];
    for (i, hz) in BAND_HZ.iter().enumerate() {
        let sum: f64 =
            p.filters.iter().map(|f| f.response_db(*hz)).sum::<f64>() + p.graphic_at(*hz);
        bands[i] = fit(
            &format!("Band {}", BAND_LABELS[i]),
            sum,
            BAND_LIMIT,
            &mut notes,
        );
    }
    let preamp = fit("Pre-amp", p.preamp_db, PREAMP_LIMIT, &mut notes);
    notes.append(&mut p.notes);

    // Belt and braces: what we hand out is writable.
    for (i, v) in bands.iter().enumerate() {
        let f = features::eq_band(i).ok_or_else(|| Error::Invalid(format!("band {i}")))?;
        f.validate_write(*v)
            .map_err(|e| Error::Invalid(e.to_string()))?;
    }
    features::by_name("eq_preamp")
        .ok_or_else(|| Error::Invalid("eq_preamp".into()))?
        .validate_write(preamp)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(EqImport {
        bands,
        preamp,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AutoEq `ParametricEQ.txt` shape: a pre-amp and ten filters. The
    /// expected bands are the summed RBJ response at the band centres,
    /// computed independently with a Python/cmath reference at 48 kHz:
    /// 4.335 3.018 0.308 -1.203 -0.099 0.387 2.646 -0.085 -3.082 -0.203 dB.
    const AUTOEQ_PARAMETRIC: &str = "Preamp: -6.4 dB\r\n\
        Filter 1: ON LSC Fc 105 Hz Gain 5.3 dB Q 0.70\r\n\
        Filter 2: ON PK Fc 50 Hz Gain -1.8 dB Q 1.03\r\n\
        Filter 3: ON PK Fc 180 Hz Gain -3.0 dB Q 1.67\r\n\
        Filter 4: ON PK Fc 2000 Hz Gain 2.9 dB Q 1.55\r\n\
        Filter 5: ON PK Fc 3200 Hz Gain -2.6 dB Q 2.78\r\n\
        Filter 6: ON PK Fc 5900 Hz Gain 4.2 dB Q 3.42\r\n\
        Filter 7: ON PK Fc 7900 Hz Gain -4.1 dB Q 4.20\r\n\
        Filter 8: ON PK Fc 11000 Hz Gain 1.5 dB Q 2.00\r\n\
        Filter 9: ON PK Fc 14000 Hz Gain 2.2 dB Q 1.10\r\n\
        Filter 10: ON HSC Fc 10000 Hz Gain -2.0 dB Q 0.70\r\n";

    #[test]
    fn autoeq_parametric_maps_to_the_pinned_bands() {
        let r = parse_eq_text(AUTOEQ_PARAMETRIC).unwrap();
        assert_eq!(
            r.bands,
            [4.5, 3.0, 0.5, -1.0, 0.0, 0.5, 2.5, 0.0, -3.0, 0.0]
        );
        // -6.4 dB is beyond the pre-amp's range.
        assert_eq!(r.preamp, -6.0);
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
        assert!(r.notes[0].contains("approximation"));
        assert!(r.notes[1].starts_with("Pre-amp: -6.4 dB clamped to -6.0 dB"));
    }

    #[test]
    fn number_handles_non_ascii_input() {
        assert_eq!(number("\u{2212}3", "db"), Some(-3.0));
        assert_eq!(number("\u{2212}3.5dB,", "db"), Some(-3.5));
        assert_eq!(number("1000Hz", "hz"), Some(1000.0));
        // Not numbers, and must not panic on a cut inside a character.
        assert_eq!(number("\u{e9}x", "db"), None);
        assert_eq!(number("\u{e9}", "db"), None);
        assert_eq!(number("\u{e9}", "hz"), None);
        assert_eq!(number("\u{4e2d}\u{6587}", "db"), None);
        assert_eq!(number("\u{2212}", "db"), None);
    }

    #[test]
    fn a_unicode_minus_gain_is_read_as_negative() {
        let r = parse_eq_text("Filter 1: ON PK Fc 1000 Hz Gain \u{2212}3 dB Q 1").unwrap();
        assert_eq!(r.bands[5], -3.0);
        let r = parse_eq_text("Preamp: \u{2212}2.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 1 dB Q 1")
            .unwrap();
        assert_eq!(r.preamp, -2.0);
    }

    #[test]
    fn non_ascii_junk_is_a_line_error_not_a_panic() {
        for text in [
            "Preamp: \u{e9}x dB",
            "Filter 1: ON PK Fc \u{e9} Hz Gain 1 dB Q 1",
            "Filter 1: ON PK Fc 1000 Hz Gain \u{e9}x Q 1",
            "Filter 1: ON PK Fc 1000 Hz Gain 1 dB Q \u{e9}",
            "GraphicEQ: \u{e9}x 1; 100 2",
            "GraphicEQ: 100 \u{e9}; 200 2",
        ] {
            assert!(parse_eq_text(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_single_peak_is_its_gain_at_its_centre() {
        // A peaking filter's gain is exact at Fc.
        let r = parse_eq_text("Filter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.0").unwrap();
        assert_eq!(r.bands[5], -3.0);
        assert_eq!(r.preamp, 0.0);
        assert_eq!(r.bands[0], 0.0);
        assert_eq!(r.bands[9], 0.0);
    }

    #[test]
    fn large_boosts_are_clamped_and_reported() {
        let r = parse_eq_text(
            "Filter 1: ON PK Fc 1000 Hz Gain 15 dB Q 1.0\nFilter 2: ON LS Fc 60 Hz Gain 9 dB Q 0.707",
        )
        .unwrap();
        // Python reference: 8.336 4.319 0.888 1.438 5.102 15.0 5.07 1.349 0.301 0.034
        assert_eq!(r.bands, [8.5, 4.5, 1.0, 1.5, 5.0, 12.0, 5.0, 1.5, 0.5, 0.0]);
        let clamped: Vec<&String> = r.notes.iter().filter(|n| n.contains("clamped")).collect();
        assert_eq!(clamped.len(), 1, "{:?}", r.notes);
        assert!(clamped[0].starts_with("Band 1 kHz: +15.0 dB clamped to +12.0 dB"));
    }

    #[test]
    fn graphic_eq_is_interpolated_over_log_frequency() {
        let r = parse_eq_text("GraphicEQ: 20 -1.0; 100 2.0; 1000 0.0; 10000 -4.0; 20000 -6.0\n")
            .unwrap();
        // Python reference: -0.183 1.109 1.806 1.204 0.602 0.0 -1.204 -2.408 -3.612 -5.356
        assert_eq!(
            r.bands,
            [0.0, 1.0, 2.0, 1.0, 0.5, 0.0, -1.0, -2.5, -3.5, -5.5]
        );
        assert_eq!(r.preamp, 0.0);
        assert!(r.notes[0].contains("GraphicEQ"));
    }

    #[test]
    fn graphic_eq_is_flat_outside_its_points_and_sorts_them() {
        let r = parse_eq_text("graphiceq: 4000 3; 40 -2; 1000 1").unwrap();
        // Below 40 Hz and above 4 kHz the end values hold.
        assert_eq!(r.bands[0], -2.0);
        assert_eq!(r.bands[1], -1.5);
        assert_eq!(r.bands[9], 3.0);
        assert_eq!(r.bands[8], 3.0);
    }

    #[test]
    fn autoeq_graphic_with_preamp_and_noise_lines() {
        let text = "# AutoEq\n\n  Preamp:  -3.5dB  \nGraphicEQ: 20 0; 31 3.0; 62 3.0; 16000 -2.0; 20000 -2.0 ;\nDevice: all\nInclude: other.txt\n";
        let r = parse_eq_text(text).unwrap();
        assert_eq!(r.preamp, -3.5);
        assert_eq!(r.bands[0], 3.0);
        assert_eq!(r.bands[1], 3.0);
        assert_eq!(r.bands[9], -2.0);
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("line 5 ignored: 'Device: all'")));
        assert!(r.notes.iter().any(|n| n.contains("line 6 ignored")));
    }

    #[test]
    fn off_filters_other_types_and_odd_spacing() {
        let text = "Preamp: -2 dB\n\
            Filter 1: OFF PK Fc 1000 Hz Gain 9 dB Q 1\n\
            Filter 2: ON HP Fc 20 Hz\n\
            filter 3:   on   pk   fc 250hz   gain 3dB   q 2\n\
            Filter 4: ON HS Fc 8000 Hz Gain -2 dB\n";
        let r = parse_eq_text(text).unwrap();
        assert_eq!(r.preamp, -2.0);
        // The OFF filter contributes nothing; the 250 Hz peak is exact at its centre.
        assert_eq!(r.bands[5], 0.0);
        assert_eq!(r.bands[3], 3.0);
        assert!(r.bands[9] < -1.0, "{:?}", r.bands);
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("line 3") && n.contains("HP")));
        assert!(r
            .notes
            .iter()
            .any(|n| n.contains("line 5") && n.contains("shelf without Q")));
    }

    #[test]
    fn preamp_lines_add_up_and_a_preamp_alone_is_an_eq() {
        let r = parse_eq_text("Preamp: -1.0 dB\nPreamp: -1.25 dB\n").unwrap();
        assert_eq!(r.bands, [0.0; 10]);
        assert_eq!(r.preamp, -2.5);
        assert!(r.notes.is_empty());
        assert!(!format!("{:?}", r.bands).contains("-0"));
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let r = parse_eq_text("\u{feff}Preamp: -1 dB").unwrap();
        assert_eq!(r.preamp, -1.0);
    }

    #[test]
    fn all_filters_off_is_a_flat_eq() {
        let r = parse_eq_text("Filter 1: OFF PK Fc 1000 Hz Gain 3 dB Q 1").unwrap();
        assert_eq!(r.bands, [0.0; 10]);
    }

    #[test]
    fn garbage_and_empty_text_are_errors() {
        for t in [
            "",
            "   \n\n",
            "hello world",
            "{\"headphones\":{}}",
            "# only a comment",
        ] {
            assert_eq!(parse_eq_text(t), Err(Error::NoEqData), "{t:?}");
        }
        let big = "#".repeat(MAX_INPUT_BYTES + 1);
        assert_eq!(parse_eq_text(&big), Err(Error::TooLarge));
    }

    #[test]
    fn broken_lines_name_the_line() {
        let cases = [
            ("Preamp: loud", 1, "pre-amp"),
            (
                "Preamp: 0 dB\nFilter 1: PK Fc 1000 Hz Gain 1 dB Q 1",
                2,
                "ON or OFF",
            ),
            ("Filter 1: ON PK Gain 1 dB Q 1", 1, "no Fc"),
            ("Filter 1: ON PK Fc 1000 Hz Q 1", 1, "no Gain"),
            ("Filter 1: ON PK Fc 1000 Hz Gain 1 dB", 1, "no Q"),
            ("Filter 1: ON PK Fc 1000 Hz Gain 1 dB BW Oct 1", 1, "BW"),
            ("Filter 1: ON PK Fc 0 Hz Gain 1 dB Q 1", 1, "Fc"),
            ("Filter 1: ON PK Fc 30000 Hz Gain 1 dB Q 1", 1, "Fc"),
            ("Filter 1: ON PK Fc 1000 Hz Gain 1 dB Q -1", 1, "Q must"),
            ("Filter 1: ON PK Fc 1000 Hz Gain 500 dB Q 1", 1, "Gain"),
            ("GraphicEQ: 31 -3; 62", 1, "GraphicEQ point"),
            ("GraphicEQ: 31 -3 4", 1, "GraphicEQ point"),
            ("GraphicEQ: ", 1, "no points"),
        ];
        for (text, line, what) in cases {
            match parse_eq_text(text) {
                Err(Error::Line { line: l, message }) => {
                    assert_eq!(l, line, "{text:?}");
                    assert!(message.contains(what), "{text:?}: {message}");
                }
                other => panic!("{text:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn results_are_always_writable() {
        for text in [
            AUTOEQ_PARAMETRIC,
            "Preamp: 40 dB\nFilter 1: ON PK Fc 16000 Hz Gain 60 dB Q 0.1",
            "GraphicEQ: 20 -99; 20000 99",
            "Filter 1: ON HSC Fc 23999 Hz Gain -100 dB Q 50",
        ] {
            let r = parse_eq_text(text).unwrap();
            for (i, v) in r.bands.iter().enumerate() {
                features::eq_band(i).unwrap().validate_write(*v).unwrap();
                assert_eq!(*v * 2.0, (*v * 2.0).round());
            }
            features::by_name("eq_preamp")
                .unwrap()
                .validate_write(r.preamp)
                .unwrap();
        }
    }
}
