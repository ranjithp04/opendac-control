//! CoreAudio backend of the mixer (macOS).
//!
//! Plain `extern "C"` declarations for the few `AudioObject*` functions used
//! (no `coreaudio-sys`: that needs bindgen and libclang at build time, and the
//! surface here is six functions and three `u32`s of address), plus the
//! `core-foundation` crate, which Tauri already pulls in, for `CFString`.
//!
//! The G6 is found by its CoreAudio model UID, which ends in `:041E:3256`
//! (USB vendor:product), with transport type USB. The `AudioObjectID` is looked
//! up on every call and never cached, since it changes on replug.
//!
//! Controls are the device-level properties: per scope, mute on element 0 and
//! volume on elements 1..N (the G6 has no master volume element); the
//! recording scope also has the data source selector. Writes set every channel
//! to the same value. See `docs/protocol-g6.md`, "Mixer".

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;
use std::sync::Mutex;

use core_foundation::base::TCFType;
use core_foundation::string::{CFString, CFStringRef};

use super::{MixerBackend, MixerError, Notify};
use crate::api::{Mixer, MixerLevel, MixerScope, MixerSource, MixerSources};

type OSStatus = i32;
type AudioObjectID = u32;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

/// `AudioValueTranslation`.
#[repr(C)]
struct ValueTranslation {
    input: *mut c_void,
    input_size: u32,
    output: *mut c_void,
    output_size: u32,
}

type ListenerProc = extern "C" fn(AudioObjectID, u32, *const Address, *mut c_void) -> OSStatus;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectHasProperty(id: AudioObjectID, address: *const Address) -> u8;
    fn AudioObjectIsPropertySettable(
        id: AudioObjectID,
        address: *const Address,
        out: *mut u8,
    ) -> OSStatus;
    fn AudioObjectGetPropertyDataSize(
        id: AudioObjectID,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        out: *mut u32,
    ) -> OSStatus;
    fn AudioObjectGetPropertyData(
        id: AudioObjectID,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        io_size: *mut u32,
        data: *mut c_void,
    ) -> OSStatus;
    fn AudioObjectSetPropertyData(
        id: AudioObjectID,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn AudioObjectAddPropertyListener(
        id: AudioObjectID,
        address: *const Address,
        listener: ListenerProc,
        client_data: *mut c_void,
    ) -> OSStatus;
}

/// FourCC constant, as written in the CoreAudio headers (`'volm'`).
const fn fcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const SYSTEM_OBJECT: AudioObjectID = 1;
const ELEMENT_MAIN: u32 = 0;
const ELEMENT_WILDCARD: u32 = 0xFFFF_FFFF;
const SCOPE_GLOBAL: u32 = fcc(b"glob");
const SCOPE_INPUT: u32 = fcc(b"inpt");
const SCOPE_OUTPUT: u32 = fcc(b"outp");
const SCOPE_PLAY_THROUGH: u32 = fcc(b"ptru");
const SCOPE_WILDCARD: u32 = fcc(b"****");
const SELECTOR_WILDCARD: u32 = fcc(b"****");
const DEVICES: u32 = fcc(b"dev#"); // kAudioHardwarePropertyDevices
const MODEL_UID: u32 = fcc(b"muid");
const TRANSPORT_TYPE: u32 = fcc(b"tran");
const TRANSPORT_USB: u32 = fcc(b"usb ");
const CONTROL_LIST: u32 = fcc(b"ctrl");
const VOLUME_SCALAR: u32 = fcc(b"volm");
const VOLUME_DECIBELS: u32 = fcc(b"vold");
const MUTE: u32 = fcc(b"mute");
const DATA_SOURCE: u32 = fcc(b"ssrc");
const DATA_SOURCES: u32 = fcc(b"ssc#");
const DATA_SOURCE_NAME: u32 = fcc(b"lscn"); // ...NameForIDCFString

/// Model UID suffix of the G6 (`Sound BlasterX G6:041E:3256`).
const G6_MODEL_SUFFIX: &str = ":041E:3256";
/// Highest channel element probed (the G6 has two).
const MAX_CHANNELS: u32 = 8;

fn addr(selector: u32, scope: u32, element: u32) -> Address {
    Address {
        selector,
        scope,
        element,
    }
}

fn scope_id(scope: MixerScope) -> u32 {
    match scope {
        MixerScope::Playback => SCOPE_OUTPUT,
        MixerScope::Monitoring => SCOPE_PLAY_THROUGH,
        MixerScope::Recording => SCOPE_INPUT,
    }
}

// --- Safe wrappers over the C calls -------------------------------------------------

fn has(id: AudioObjectID, a: Address) -> bool {
    // SAFETY: `a` outlives the call; the function only reads it.
    unsafe { AudioObjectHasProperty(id, &a) != 0 }
}

fn settable(id: AudioObjectID, a: Address) -> bool {
    let mut out = 0u8;
    // SAFETY: both pointers are valid for the call.
    let r = unsafe { AudioObjectIsPropertySettable(id, &a, &mut out) };
    r == 0 && out != 0
}

/// Read a property of plain-old-data type `T` (`init` is overwritten).
fn get<T: Copy>(id: AudioObjectID, a: Address, init: T) -> Result<T, OSStatus> {
    let mut value = init;
    let mut size = size_of::<T>() as u32;
    // SAFETY: `value` is `size` bytes of writable memory and a valid `T` for
    // the properties this module reads (u32 / f32 / CFStringRef).
    let r = unsafe {
        AudioObjectGetPropertyData(
            id,
            &a,
            0,
            ptr::null(),
            &mut size,
            (&mut value as *mut T).cast(),
        )
    };
    if r == 0 {
        Ok(value)
    } else {
        Err(r)
    }
}

/// Read an array property of `u32` (device ids, control ids, data source ids).
fn get_ids(id: AudioObjectID, a: Address) -> Result<Vec<u32>, OSStatus> {
    let mut size = 0u32;
    // SAFETY: valid pointers for the call.
    let r = unsafe { AudioObjectGetPropertyDataSize(id, &a, 0, ptr::null(), &mut size) };
    if r != 0 {
        return Err(r);
    }
    let mut out = vec![0u32; size as usize / size_of::<u32>()];
    let mut size = (out.len() * size_of::<u32>()) as u32;
    // SAFETY: `out` has `size` writable bytes.
    let r = unsafe {
        AudioObjectGetPropertyData(id, &a, 0, ptr::null(), &mut size, out.as_mut_ptr().cast())
    };
    if r != 0 {
        return Err(r);
    }
    out.truncate(size as usize / size_of::<u32>());
    Ok(out)
}

fn set<T: Copy>(id: AudioObjectID, a: Address, value: T) -> Result<(), OSStatus> {
    // SAFETY: `value` is `size_of::<T>()` readable bytes for the call.
    let r = unsafe {
        AudioObjectSetPropertyData(
            id,
            &a,
            0,
            ptr::null(),
            size_of::<T>() as u32,
            (&value as *const T).cast(),
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(r)
    }
}

/// Take ownership of a CFString the HAL returned (create rule: retained for us).
fn cf_string(raw: CFStringRef) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: non-null, and the HAL hands out a +1 reference for these properties.
    Some(unsafe { CFString::wrap_under_create_rule(raw) }.to_string())
}

fn string_prop(id: AudioObjectID, a: Address) -> Option<String> {
    cf_string(get(id, a, ptr::null() as CFStringRef).ok()?)
}

// --- Device and controls ----------------------------------------------------------

fn is_g6(id: AudioObjectID) -> bool {
    let usb = get(id, addr(TRANSPORT_TYPE, SCOPE_GLOBAL, ELEMENT_MAIN), 0u32) == Ok(TRANSPORT_USB);
    usb && string_prop(id, addr(MODEL_UID, SCOPE_GLOBAL, ELEMENT_MAIN))
        .is_some_and(|m| m.to_ascii_uppercase().ends_with(G6_MODEL_SUFFIX))
}

fn find_g6() -> Option<AudioObjectID> {
    get_ids(SYSTEM_OBJECT, addr(DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN))
        .ok()?
        .into_iter()
        .find(|&id| is_g6(id))
}

/// Channel elements (1..) of `scope` that have a volume.
fn channels(dev: AudioObjectID, scope: u32) -> Vec<u32> {
    (1..=MAX_CHANNELS)
        .take_while(|&el| has(dev, addr(VOLUME_SCALAR, scope, el)))
        .collect()
}

fn mean(values: &[f32]) -> Option<f32> {
    (!values.is_empty()).then(|| values.iter().sum::<f32>() / values.len() as f32)
}

fn read_level(dev: AudioObjectID, scope: u32) -> Option<MixerLevel> {
    let els = channels(dev, scope);
    let vols: Vec<f32> = els
        .iter()
        .filter_map(|&el| get(dev, addr(VOLUME_SCALAR, scope, el), 0f32).ok())
        .collect();
    let dbs: Vec<f32> = els
        .iter()
        .filter_map(|&el| get(dev, addr(VOLUME_DECIBELS, scope, el), 0f32).ok())
        .collect();
    let mute_addr = addr(MUTE, scope, ELEMENT_MAIN);
    Some(MixerLevel {
        volume: mean(&vols)?,
        volume_db: mean(&dbs),
        mute: if has(dev, mute_addr) {
            get(dev, mute_addr, 0u32).ok().map(|m| m != 0)
        } else {
            None
        },
    })
}

fn source_name(dev: AudioObjectID, id: u32) -> String {
    let mut source = id;
    let mut name: CFStringRef = ptr::null();
    let mut t = ValueTranslation {
        input: (&mut source as *mut u32).cast(),
        input_size: size_of::<u32>() as u32,
        output: (&mut name as *mut CFStringRef).cast(),
        output_size: size_of::<CFStringRef>() as u32,
    };
    let mut size = size_of::<ValueTranslation>() as u32;
    let a = addr(DATA_SOURCE_NAME, SCOPE_INPUT, ELEMENT_MAIN);
    // SAFETY: `t` points at `source` and `name`, both alive and writable for
    // the call; `size` is the size of `t`.
    let r = unsafe {
        AudioObjectGetPropertyData(
            dev,
            &a,
            0,
            ptr::null(),
            &mut size,
            (&mut t as *mut ValueTranslation).cast(),
        )
    };
    if r == 0 {
        if let Some(n) = cf_string(name) {
            return n;
        }
    }
    format!("Source {id}")
}

fn read_sources(dev: AudioObjectID) -> Option<MixerSources> {
    let current = get(dev, addr(DATA_SOURCE, SCOPE_INPUT, ELEMENT_MAIN), 0u32).ok()?;
    let ids = get_ids(dev, addr(DATA_SOURCES, SCOPE_INPUT, ELEMENT_MAIN)).ok()?;
    Some(MixerSources {
        current,
        options: ids
            .into_iter()
            .map(|id| MixerSource {
                id,
                name: source_name(dev, id),
            })
            .collect(),
    })
}

fn read_mixer(dev: AudioObjectID) -> Mixer {
    Mixer {
        playback: read_level(dev, SCOPE_OUTPUT),
        monitoring: read_level(dev, SCOPE_PLAY_THROUGH),
        recording: read_level(dev, SCOPE_INPUT),
        recording_source: read_sources(dev),
    }
}

// --- Change notifications ---------------------------------------------------------

/// Client data of the listeners: a leaked `Box<Notify>`. It lives as long as
/// the process (the listeners are never removed), so the pointer stays valid.
extern "C" fn on_change(
    _object: AudioObjectID,
    _count: u32,
    _addresses: *const Address,
    client_data: *mut c_void,
) -> OSStatus {
    // SAFETY: `client_data` is the leaked `Box<Notify>` made in `subscribe`
    // and never freed; it is only read through a shared reference.
    let notify = unsafe { &*(client_data as *const Notify) };
    notify();
    0
}

/// Register `on_change` for `a`; false (and a warning) if CoreAudio refuses.
fn listen(id: AudioObjectID, a: Address, data: *mut c_void) -> bool {
    // SAFETY: `a` is valid for the call, `on_change` has the C ABI, and
    // `data` is the leaked box described at `on_change`.
    let r = unsafe { AudioObjectAddPropertyListener(id, &a, on_change, data) };
    if r != 0 {
        log::warn!("cannot listen for audio changes on object {id}: status {r}");
    }
    r == 0
}

struct Watch {
    /// The leaked `Box<Notify>` as an address (so `Watch` is `Send`).
    data: usize,
    /// Device the property listeners are registered on.
    device: Option<AudioObjectID>,
}

/// Mixer on top of CoreAudio. Stateless apart from the change subscription.
pub struct CoreAudioMixer {
    watch: Mutex<Option<Watch>>,
}

impl CoreAudioMixer {
    pub fn new() -> CoreAudioMixer {
        CoreAudioMixer {
            watch: Mutex::new(None),
        }
    }

    /// Make sure the listeners sit on the current G6 audio device (its id
    /// changes when it is replugged). Listeners on a vanished device die with it.
    fn rewatch(&self, current: Option<AudioObjectID>) {
        let mut guard = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        let Some(w) = guard.as_mut() else { return };
        if w.device == current {
            return;
        }
        let Some(dev) = current else {
            w.device = None;
            return;
        };
        let data = w.data as *mut c_void;
        let all = addr(SELECTOR_WILDCARD, SCOPE_WILDCARD, ELEMENT_WILDCARD);
        // The device (legacy per-scope properties) and its control objects
        // (volume, mute, selector values): whichever the driver notifies on.
        // Recorded as watched only once the device took its listener, so a
        // refusal (a device still being published) is retried on the next
        // read; a refused control object is only logged.
        if !listen(dev, all, data) {
            w.device = None;
            return;
        }
        w.device = current;
        let controls = get_ids(dev, addr(CONTROL_LIST, SCOPE_GLOBAL, ELEMENT_MAIN));
        for ctl in controls.unwrap_or_default() {
            listen(ctl, all, data);
        }
    }

    fn device(&self) -> Result<AudioObjectID, MixerError> {
        find_g6().ok_or_else(|| MixerError::Unavailable("the G6 audio device was not found".into()))
    }
}

impl Default for CoreAudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

fn failed(what: &str, status: OSStatus) -> MixerError {
    MixerError::Failed(format!("CoreAudio could not set {what} (status {status})"))
}

impl MixerBackend for CoreAudioMixer {
    fn read(&self) -> Option<Mixer> {
        let dev = find_g6();
        self.rewatch(dev);
        dev.map(read_mixer)
    }

    fn set_volume(&self, scope: MixerScope, volume: f32) -> Result<(), MixerError> {
        let dev = self.device()?;
        let sc = scope_id(scope);
        let els = channels(dev, sc);
        if els.is_empty() {
            return Err(MixerError::Unavailable(format!(
                "no {scope:?} volume control"
            )));
        }
        if let Some(&el) = els
            .iter()
            .find(|&&el| !settable(dev, addr(VOLUME_SCALAR, sc, el)))
        {
            return Err(MixerError::Unavailable(format!(
                "the {scope:?} volume (channel {el}) is read-only"
            )));
        }
        for el in els {
            set(dev, addr(VOLUME_SCALAR, sc, el), volume).map_err(|s| failed("the volume", s))?;
        }
        Ok(())
    }

    fn set_mute(&self, scope: MixerScope, mute: bool) -> Result<(), MixerError> {
        let dev = self.device()?;
        let a = addr(MUTE, scope_id(scope), ELEMENT_MAIN);
        if !has(dev, a) || !settable(dev, a) {
            return Err(MixerError::Unavailable(format!(
                "no writable {scope:?} mute"
            )));
        }
        set(dev, a, u32::from(mute)).map_err(|s| failed("the mute", s))
    }

    fn set_source(&self, id: u32) -> Result<(), MixerError> {
        let dev = self.device()?;
        let a = addr(DATA_SOURCE, SCOPE_INPUT, ELEMENT_MAIN);
        if !has(dev, a) || !settable(dev, a) {
            return Err(MixerError::Unavailable(
                "no writable recording source".into(),
            ));
        }
        set(dev, a, id).map_err(|s| failed("the recording source", s))
    }

    fn subscribe(&self, notify: Notify) {
        let mut guard = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return;
        }
        // Leaked on purpose: CoreAudio keeps the pointer for the process lifetime.
        let data = Box::into_raw(Box::new(notify)).cast::<c_void>();
        *guard = Some(Watch {
            data: data as usize,
            device: None,
        });
        drop(guard);
        // Devices appearing / disappearing (replug).
        listen(
            SYSTEM_OBJECT,
            addr(DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN),
            data,
        );
        self.rewatch(find_g6());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_char_codes_match_the_headers() {
        assert_eq!(SCOPE_INPUT, 0x696e_7074);
        assert_eq!(SCOPE_PLAY_THROUGH, 0x7074_7275);
        assert_eq!(TRANSPORT_USB, 0x7573_6220);
        assert_eq!(ELEMENT_WILDCARD, u32::MAX);
    }

    #[test]
    fn mean_of_channels() {
        assert_eq!(mean(&[]), None);
        assert_eq!(mean(&[0.25, 0.75]), Some(0.5));
    }

    #[test]
    fn scopes_map_to_coreaudio_scopes() {
        assert_eq!(scope_id(MixerScope::Playback), SCOPE_OUTPUT);
        assert_eq!(scope_id(MixerScope::Monitoring), SCOPE_PLAY_THROUGH);
        assert_eq!(scope_id(MixerScope::Recording), SCOPE_INPUT);
    }

    /// Read-only: finds a plugged-in G6 and reads its controls. Never sets
    /// anything. `cargo test -p opendac-control coreaudio -- --ignored`
    #[test]
    #[ignore = "needs a G6 plugged in"]
    fn reads_the_real_g6_without_writing() {
        let m = CoreAudioMixer::new().read().expect("G6 audio device");
        let play = m.playback.expect("playback");
        assert!((0.0..=1.0).contains(&play.volume));
        assert!(m.recording.is_some() && m.monitoring.is_some());
        let src = m.recording_source.as_ref().expect("source selector");
        assert!(src.options.iter().any(|o| o.id == src.current));
        eprintln!("{m:#?}");
    }

    /// Registers the change listeners (they are read-only hooks) and checks
    /// that none was refused. Whether they fire on a knob turn needs a human.
    #[test]
    #[ignore = "needs a G6 plugged in"]
    fn listeners_register_on_the_real_g6() {
        let dev = find_g6().expect("G6 audio device");
        let notify: Notify = Box::new(|| {});
        let data = Box::into_raw(Box::new(notify)).cast::<c_void>();
        let all = addr(SELECTOR_WILDCARD, SCOPE_WILDCARD, ELEMENT_WILDCARD);
        assert!(listen(dev, all, data), "device");
        let controls = get_ids(dev, addr(CONTROL_LIST, SCOPE_GLOBAL, ELEMENT_MAIN)).unwrap();
        assert!(!controls.is_empty());
        for c in controls {
            assert!(listen(c, all, data), "control {c}");
        }
        assert!(listen(
            SYSTEM_OBJECT,
            addr(DEVICES, SCOPE_GLOBAL, ELEMENT_MAIN),
            data
        ));
        // The real path too.
        let mixer = CoreAudioMixer::new();
        mixer.subscribe(Box::new(|| {}));
        let watch = mixer.watch.lock().unwrap();
        assert!(watch.as_ref().unwrap().device.is_some());
    }
}
