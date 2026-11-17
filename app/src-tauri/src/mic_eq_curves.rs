//! User-saved mic EQ curves: the 8 mic EQ band gains under a name, stored in
//! `mic_eq_curves.json` in the config dir (next to `eq_curves.json`).
//!
//! The built-in presets live in the UI (`app/ui/src/lib/micEqCurves.ts`);
//! only their names are known here, so a user curve cannot shadow one. Name
//! rules, limits and errors are those of the playback EQ curves.
//!
//! File format: `{ "curves": [ { "name": "Mine", "bands": [..8..] } ] }`,
//! in the order the curves were first saved.

use std::path::{Path, PathBuf};

use g6_core::store::{self, StoreError};
use serde::{Deserialize, Serialize};

use crate::eq_curves::{same_name, validate_name_with, CurveError, MAX_CURVES};

/// File name in the config dir.
pub const FILE: &str = "mic_eq_curves.json";

/// Names the UI uses for its own chips; a user curve cannot take them
/// (compared case-insensitively). `Custom` is the "no chip matches" chip.
pub const RESERVED_NAMES: [&str; 12] = [
    "Preset 1",
    "Preset 2",
    "Preset 3",
    "Preset 4",
    "Preset 5",
    "Preset 6",
    "Preset 7",
    "Preset 8",
    "Preset 9",
    "Preset 10",
    "Preset 11",
    "Custom",
];

/// A saved mic EQ curve. Gains are dB (-12..=12, 0.5 steps).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicEqCurve {
    pub name: String,
    pub bands: [f32; 8],
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileContents {
    #[serde(default)]
    curves: Vec<MicEqCurve>,
}

/// Trim `name` and check it can be used for a saved mic EQ curve.
pub fn validate_name(name: &str) -> Result<String, CurveError> {
    validate_name_with(name, &RESERVED_NAMES)
}

/// The mic curves file.
#[derive(Debug, Clone)]
pub struct MicEqCurveStore {
    path: PathBuf,
}

impl MicEqCurveStore {
    /// Store in `config_dir`.
    pub fn in_dir(config_dir: &Path) -> Self {
        MicEqCurveStore {
            path: config_dir.join(FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// All curves in saved order; no file means none.
    pub fn list(&self) -> Result<Vec<MicEqCurve>, StoreError> {
        Ok(store::read_json::<FileContents>(&self.path)?
            .map(|f| f.curves)
            .unwrap_or_default())
    }

    fn write(&self, curves: Vec<MicEqCurve>) -> Result<(), StoreError> {
        store::write_json(&self.path, &FileContents { curves })
    }

    /// Save under `name` (trimmed; names are unique ignoring case, saving an
    /// existing name replaces that curve in place). The values are stored as
    /// given: the caller validates them against the feature table.
    pub fn save(&self, name: &str, bands: [f32; 8]) -> Result<(), CurveError> {
        let name = validate_name(name)?;
        let mut curves = self.list()?;
        let curve = MicEqCurve {
            name: name.clone(),
            bands,
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
                "opendac-mic-curves-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BANDS: [f32; 8] = [1.0, 2.0, 3.0, 0.0, -1.5, -2.0, 0.5, 4.0];

    fn store(dir: &TempDir) -> MicEqCurveStore {
        MicEqCurveStore::in_dir(&dir.0)
    }

    #[test]
    fn save_list_delete_round_trip_in_saved_order() {
        let dir = TempDir::new();
        let s = store(&dir);
        assert!(s.list().unwrap().is_empty(), "no file means none");
        s.save("Zebra", BANDS).unwrap();
        s.save("  Alpha  ", [0.0; 8]).unwrap();
        let list = s.list().unwrap();
        assert_eq!(
            list.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Zebra", "Alpha"]
        );
        assert_eq!(list[0].bands, BANDS);

        // Same name (ignoring case) replaces in place.
        s.save("zebra", [1.0; 8]).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "zebra");
        assert_eq!(list[0].bands, [1.0; 8]);

        s.delete("ZEBRA").unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        assert!(matches!(s.delete("zebra"), Err(CurveError::NotFound(_))));
    }

    #[test]
    fn names_are_checked() {
        let dir = TempDir::new();
        let s = store(&dir);
        let bad = |n: &str| matches!(s.save(n, [0.0; 8]), Err(CurveError::BadName(_)));
        assert!(bad(""));
        assert!(bad(&"x".repeat(crate::eq_curves::MAX_NAME_LEN + 1)));
        assert!(bad("a\nb"));
        for r in RESERVED_NAMES {
            assert!(bad(r), "{r}");
            assert!(bad(&r.to_uppercase()), "{r} upper");
        }
        assert!(s.list().unwrap().is_empty(), "nothing was stored");
        // The playback EQ's names are free here.
        s.save("Flat", [0.0; 8]).unwrap();
    }

    #[test]
    fn number_of_curves_is_capped() {
        let dir = TempDir::new();
        let s = store(&dir);
        for i in 0..MAX_CURVES {
            s.save(&format!("c{i}"), [0.0; 8]).unwrap();
        }
        assert!(matches!(
            s.save("one more", [0.0; 8]),
            Err(CurveError::TooMany)
        ));
        s.save("c3", BANDS).unwrap();
    }

    #[test]
    fn corrupt_file_is_a_storage_error_and_file_shape() {
        let dir = TempDir::new();
        let s = store(&dir);
        s.save("Mine", BANDS).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(s.path()).unwrap()).unwrap();
        assert_eq!(v["curves"][0]["name"], "Mine");
        assert_eq!(v["curves"][0]["bands"].as_array().unwrap().len(), 8);

        std::fs::write(s.path(), "{ not json").unwrap();
        assert!(s.list().is_err());
        assert!(matches!(s.save("A", [0.0; 8]), Err(CurveError::Store(_))));
    }
}
