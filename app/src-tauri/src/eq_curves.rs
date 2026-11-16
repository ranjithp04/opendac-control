//! User-saved EQ curves: 10 band gains plus the pre-amp under a name,
//! stored in `eq_curves.json` in the config dir (next to `app_settings.json`).
//!
//! The built-in curves live in the UI (`app/ui/src/lib/eqCurves.ts`); only
//! their names are known here, so a user curve cannot shadow one.
//!
//! File format: `{ "curves": [ { "name": "Mine", "bands": [..10..], "preamp": -2.5 } ] }`,
//! in the order the curves were first saved.

use std::fmt;
use std::path::{Path, PathBuf};

use g6_core::store::{self, StoreError};
use serde::{Deserialize, Serialize};

use crate::api::{ApiError, ErrorCode};

/// File name in the config dir.
pub const FILE: &str = "eq_curves.json";

/// Longest curve name, in characters (the name is shown on a chip).
pub const MAX_NAME_LEN: usize = 32;

/// Most user curves kept.
pub const MAX_CURVES: usize = 32;

/// Names the UI uses for its own chips; a user curve cannot take them
/// (compared case-insensitively). `Custom` is the "no chip matches" chip.
pub const RESERVED_NAMES: [&str; 11] = [
    "Flat",
    "Bass boost",
    "Bass cut",
    "Treble boost",
    "Warm",
    "V-shape",
    "Loudness",
    "Speech",
    "Footsteps",
    "Small speakers",
    "Custom",
];

/// A saved curve. Gains are dB (bands -12..=12, pre-amp -6..=6, 0.5 steps).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EqCurve {
    pub name: String,
    pub bands: [f32; 10],
    pub preamp: f32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileContents {
    #[serde(default)]
    curves: Vec<EqCurve>,
}

#[derive(Debug)]
pub enum CurveError {
    BadName(String),
    TooMany,
    NotFound(String),
    Store(StoreError),
}

impl fmt::Display for CurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CurveError::BadName(m) => f.write_str(m),
            CurveError::TooMany => write!(f, "you can keep at most {MAX_CURVES} saved curves"),
            CurveError::NotFound(n) => write!(f, "no saved curve named \"{n}\""),
            CurveError::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for CurveError {}

impl From<StoreError> for CurveError {
    fn from(e: StoreError) -> Self {
        CurveError::Store(e)
    }
}

impl From<CurveError> for ApiError {
    fn from(e: CurveError) -> Self {
        let code = match &e {
            CurveError::BadName(_) | CurveError::TooMany => ErrorCode::InvalidValue,
            CurveError::NotFound(_) => ErrorCode::CurveNotFound,
            CurveError::Store(_) => ErrorCode::Storage,
        };
        ApiError::new(code, e.to_string())
    }
}

/// Trim `name` and check it can be used for a saved curve.
pub fn validate_name(name: &str) -> Result<String, CurveError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CurveError::BadName("give the curve a name".into()));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(CurveError::BadName(format!(
            "curve names are at most {MAX_NAME_LEN} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(CurveError::BadName(
            "curve names cannot contain control characters".into(),
        ));
    }
    if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        return Err(CurveError::BadName(format!(
            "\"{name}\" is a built-in curve name"
        )));
    }
    Ok(name.to_string())
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// The curves file.
#[derive(Debug, Clone)]
pub struct EqCurveStore {
    path: PathBuf,
}

impl EqCurveStore {
    pub fn new(path: PathBuf) -> Self {
        EqCurveStore { path }
    }

    /// Store in `config_dir`.
    pub fn in_dir(config_dir: &Path) -> Self {
        Self::new(config_dir.join(FILE))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// All curves in saved order; no file means none.
    pub fn list(&self) -> Result<Vec<EqCurve>, StoreError> {
        Ok(store::read_json::<FileContents>(&self.path)?
            .map(|f| f.curves)
            .unwrap_or_default())
    }

    fn write(&self, curves: Vec<EqCurve>) -> Result<(), StoreError> {
        store::write_json(&self.path, &FileContents { curves })
    }

    /// Save under `name` (trimmed; names are unique ignoring case, saving an
    /// existing name replaces that curve in place). The values are stored as
    /// given: the caller validates them against the feature table.
    pub fn save(&self, name: &str, bands: [f32; 10], preamp: f32) -> Result<(), CurveError> {
        let name = validate_name(name)?;
        let mut curves = self.list()?;
        let curve = EqCurve {
            name: name.clone(),
            bands,
            preamp,
        };
        match curves.iter().position(|c| same_name(&c.name, &name)) {
            Some(i) => curves[i] = curve,
            None if curves.len() >= MAX_CURVES => return Err(CurveError::TooMany),
            None => curves.push(curve),
        }
        Ok(self.write(curves)?)
    }

    /// Delete the curve named `name` (ignoring case).
    pub fn delete(&self, name: &str) -> Result<(), CurveError> {
        let mut curves = self.list()?;
        let before = curves.len();
        curves.retain(|c| !same_name(&c.name, name.trim()));
        if curves.len() == before {
            return Err(CurveError::NotFound(name.to_string()));
        }
        Ok(self.write(curves)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "opendac-curves-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BANDS: [f32; 10] = [1.0, 2.0, 3.0, 0.0, -1.5, -2.0, 0.5, 4.0, 5.0, -6.0];

    fn store(dir: &TempDir) -> EqCurveStore {
        EqCurveStore::in_dir(dir.path())
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = TempDir::new();
        assert!(store(&dir).list().unwrap().is_empty());
    }

    #[test]
    fn save_list_delete_round_trip_in_saved_order() {
        let dir = TempDir::new();
        let s = store(&dir);
        s.save("Zebra", BANDS, -2.5).unwrap();
        s.save("  Alpha  ", [0.0; 10], 0.0).unwrap();
        let list = s.list().unwrap();
        assert_eq!(
            list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Zebra", "Alpha"],
            "saved order, name trimmed"
        );
        assert_eq!(list[0].bands, BANDS);
        assert_eq!(list[0].preamp, -2.5);

        s.delete("zebra").unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        assert!(matches!(s.delete("zebra"), Err(CurveError::NotFound(_))));
    }

    #[test]
    fn saving_an_existing_name_replaces_it_in_place() {
        let dir = TempDir::new();
        let s = store(&dir);
        s.save("A", [0.0; 10], 0.0).unwrap();
        s.save("B", [0.0; 10], 0.0).unwrap();
        s.save("a", BANDS, -1.0).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "a");
        assert_eq!(list[0].bands, BANDS);
        assert_eq!(list[1].name, "B");
    }

    #[test]
    fn names_are_checked() {
        let dir = TempDir::new();
        let s = store(&dir);
        let bad = |n: &str| matches!(s.save(n, [0.0; 10], 0.0), Err(CurveError::BadName(_)));
        assert!(bad(""));
        assert!(bad("   "));
        assert!(bad(&"x".repeat(MAX_NAME_LEN + 1)));
        assert!(bad("a\nb"));
        for r in RESERVED_NAMES {
            assert!(bad(r), "{r}");
            assert!(bad(&r.to_uppercase()), "{r} upper");
        }
        assert!(s.list().unwrap().is_empty(), "nothing was stored");
        s.save(&"x".repeat(MAX_NAME_LEN), [0.0; 10], 0.0).unwrap();
    }

    #[test]
    fn number_of_curves_is_capped() {
        let dir = TempDir::new();
        let s = store(&dir);
        for i in 0..MAX_CURVES {
            s.save(&format!("c{i}"), [0.0; 10], 0.0).unwrap();
        }
        assert!(matches!(
            s.save("one more", [0.0; 10], 0.0),
            Err(CurveError::TooMany)
        ));
        // Replacing one is still fine.
        s.save("c3", BANDS, 1.0).unwrap();
    }

    #[test]
    fn corrupt_file_is_a_storage_error_not_a_panic() {
        let dir = TempDir::new();
        let s = store(&dir);
        std::fs::write(s.path(), "{ not json").unwrap();
        assert!(s.list().is_err());
        assert!(matches!(
            s.save("A", [0.0; 10], 0.0),
            Err(CurveError::Store(_))
        ));
        assert_eq!(
            ApiError::from(CurveError::NotFound("x".into())).code,
            ErrorCode::CurveNotFound
        );
    }

    #[test]
    fn file_shape() {
        let dir = TempDir::new();
        let s = store(&dir);
        s.save("Mine", BANDS, -2.5).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(s.path()).unwrap()).unwrap();
        assert_eq!(v["curves"][0]["name"], "Mine");
        assert_eq!(v["curves"][0]["preamp"], -2.5);
        assert_eq!(v["curves"][0]["bands"].as_array().unwrap().len(), 10);
    }
}
