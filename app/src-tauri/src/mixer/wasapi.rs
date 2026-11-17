//! Core Audio (WASAPI) backend of the mixer (Windows).
//!
//! Windows presents the G6's USB audio function as audio *endpoints*: one or
//! more render endpoints (playback) and capture endpoints (recording). The
//! endpoint's `IAudioEndpointVolume` is the master volume and mute that the
//! Sound control panel shows; the class driver maps it to the USB feature
//! unit on the endpoint's signal path. That gives playback and recording.
//!
//! The recording source is the input selector (`IAudioInputSelector`) found by
//! walking the capture endpoint's device topology upstream, toward the
//! hardware. Windows has no generic "monitoring" (play-through) control for a
//! USB device, so `monitoring` is always `null` here.
//!
//! The G6's endpoints are found by the USB id (`vid_041e&pid_3256`) in the
//! device id the endpoint's connector is attached to, falling back to the
//! friendly name. Nothing is cached: endpoints are enumerated on every call
//! (a replug gives new ones). See `docs/protocol-g6.md`, "Mixer".

use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use windows::core::{implement, Interface, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, ERole, IAudioInputSelector, IConnector,
    IDeviceTopology, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, IPart, MMDeviceEnumerator, Subunit, AUDIO_VOLUME_NOTIFICATION_DATA,
    DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};

use super::{MixerBackend, MixerError, Notify};
use crate::api::{Mixer, MixerLevel, MixerScope, MixerSource, MixerSources};

/// USB vendor and product id as Windows writes them in a device interface path.
const G6_USB_ID: &str = "vid_041e&pid_3256";
/// Lower-case parts of the friendly name, used when the id has no USB path.
const G6_NAMES: [&str; 2] = ["blasterx g6", "sbx g6"];
/// Deepest part chain followed when looking for the input selector.
const MAX_TOPOLOGY_DEPTH: u32 = 16;

fn failed(what: &str, e: windows::core::Error) -> MixerError {
    MixerError::Failed(format!("{what}: {e}"))
}

fn unavailable(what: &str) -> MixerError {
    MixerError::Unavailable(what.into())
}

// --- COM ----------------------------------------------------------------

/// COM initialised on this thread (multi-threaded apartment), undone when the thread ends.
struct Com(bool);

impl Com {
    fn init() -> Com {
        // S_OK and S_FALSE both need a matching CoUninitialize; a failure (the
        // thread is already in another apartment) does not.
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static COM: Com = Com::init();
}

/// Run `f` on a thread that has COM initialised.
fn with_com<T>(f: impl FnOnce() -> T) -> T {
    COM.with(|_| f())
}

fn enumerator() -> Result<IMMDeviceEnumerator, MixerError> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| failed("audio device enumerator", e))
}

/// Take a string COM allocated.
fn take_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

// --- Finding the G6 -----------------------------------------------------

/// Whether a device interface path (lower case) is the G6's USB audio function.
fn is_g6_id(id: &str) -> bool {
    id.to_lowercase().contains(G6_USB_ID)
}

/// Whether a friendly name (for example `Speakers (Sound BlasterX G6)`) is a G6 endpoint.
fn is_g6_name(name: &str) -> bool {
    let name = name.to_lowercase();
    G6_NAMES.iter().any(|n| name.contains(n))
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        Some(take_string(PropVariantToStringAlloc(&value).ok()?))
    }
}

/// The id of the audio adapter the endpoint is attached to.
fn adapter_id(device: &IMMDevice) -> Option<String> {
    unsafe {
        let topology: IDeviceTopology = device.Activate(CLSCTX_ALL, None).ok()?;
        let connector = topology.GetConnector(0).ok()?;
        Some(take_string(connector.GetDeviceIdConnectedTo().ok()?))
    }
}

fn is_g6(device: &IMMDevice) -> bool {
    adapter_id(device).is_some_and(|id| is_g6_id(&id))
        || friendly_name(device).is_some_and(|n| is_g6_name(&n))
}

fn device_id(device: &IMMDevice) -> Option<String> {
    unsafe { device.GetId().ok() }.map(take_string)
}

/// The G6's active endpoint for `flow`: the system default one when that is a G6, else the first.
fn g6_endpoint(en: &IMMDeviceEnumerator, flow: EDataFlow) -> Option<IMMDevice> {
    let list = unsafe { en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }.ok()?;
    let count = unsafe { list.GetCount() }.ok()?;
    let found: Vec<IMMDevice> = (0..count)
        .filter_map(|i| unsafe { list.Item(i) }.ok())
        .filter(is_g6)
        .collect();
    let default = unsafe { en.GetDefaultAudioEndpoint(flow, eConsole) }
        .ok()
        .and_then(|d| device_id(&d));
    let preferred = default.and_then(|id| {
        found
            .iter()
            .position(|d| device_id(d).as_ref() == Some(&id))
    });
    found.into_iter().nth(preferred.unwrap_or(0))
}

fn volume_of(device: &IMMDevice) -> Result<IAudioEndpointVolume, MixerError> {
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }
        .map_err(|e| failed("endpoint volume", e))
}

fn level(device: &IMMDevice) -> Option<MixerLevel> {
    let v = volume_of(device).ok()?;
    unsafe {
        Some(MixerLevel {
            volume: v.GetMasterVolumeLevelScalar().ok()?,
            volume_db: v.GetMasterVolumeLevel().ok(),
            mute: v.GetMute().ok().map(|m| m.as_bool()),
        })
    }
}

// --- Recording source ---------------------------------------------------

/// The input selector upstream of `part` (toward the hardware), and the part it is.
fn find_selector(part: &IPart, depth: u32) -> Option<(IAudioInputSelector, IPart)> {
    if depth > MAX_TOPOLOGY_DEPTH {
        return None;
    }
    if unsafe { part.GetPartType() }.is_ok_and(|t| t == Subunit) {
        let mut raw: *mut c_void = ptr::null_mut();
        let activated =
            unsafe { part.Activate(CLSCTX_ALL.0, &IAudioInputSelector::IID, Some(&mut raw)) };
        if activated.is_ok() && !raw.is_null() {
            return Some((unsafe { IAudioInputSelector::from_raw(raw) }, part.clone()));
        }
    }
    let upstream = unsafe { part.EnumPartsIncoming() }.ok()?;
    let count = unsafe { upstream.GetCount() }.ok()?;
    (0..count)
        .filter_map(|i| unsafe { upstream.GetPart(i) }.ok())
        .find_map(|p| find_selector(&p, depth + 1))
}

/// The connector of an endpoint's signal path on the adapter side, as a part.
fn adapter_part(device: &IMMDevice) -> Option<IPart> {
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    let own: IConnector = unsafe { topology.GetConnector(0) }.ok()?;
    let adapter_side: IConnector = unsafe { own.GetConnectedTo() }.ok()?;
    adapter_side.cast().ok()
}

/// The selector of a capture endpoint, with the part that holds it.
fn selector_of(device: &IMMDevice) -> Option<(IAudioInputSelector, IPart)> {
    find_selector(&adapter_part(device)?, 0)
}

/// The inputs a selector chooses between: the local id and name of each part feeding it.
fn selector_options(part: &IPart) -> Vec<MixerSource> {
    let Ok(list) = (unsafe { part.EnumPartsIncoming() }) else {
        return Vec::new();
    };
    let count = unsafe { list.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|i| {
            let p = unsafe { list.GetPart(i) }.ok()?;
            let id = unsafe { p.GetLocalId() }.ok()?;
            let name = take_string(unsafe { p.GetName() }.ok()?);
            Some(MixerSource {
                id,
                name: if name.is_empty() {
                    format!("Input {id}")
                } else {
                    name
                },
            })
        })
        .collect()
}

fn sources(device: &IMMDevice) -> Option<MixerSources> {
    let (selector, part) = selector_of(device)?;
    let options = selector_options(&part);
    if options.is_empty() {
        return None;
    }
    let current = unsafe { selector.GetSelection() }.ok()?;
    Some(MixerSources { current, options })
}

// --- Diagnostic dump ----------------------------------------------------

fn dump_part(out: &mut String, part: &IPart, depth: usize, incoming: bool, seen: &mut Vec<String>) {
    use std::fmt::Write;
    let indent = "  ".repeat(depth);
    let id = unsafe { part.GetGlobalId() }
        .map(take_string)
        .unwrap_or_default();
    if depth > MAX_TOPOLOGY_DEPTH as usize || seen.contains(&id) {
        return;
    }
    seen.push(id.clone());
    let name = unsafe { part.GetName() }
        .map(take_string)
        .unwrap_or_default();
    let kind = match unsafe { part.GetPartType() } {
        Ok(t) if t == Subunit => "subunit",
        Ok(_) => "connector",
        Err(_) => "?",
    };
    let local = unsafe { part.GetLocalId() }.unwrap_or(0);
    let sub = unsafe { part.GetSubType() }
        .map(|g| format!("{g:?}"))
        .unwrap_or_default();
    let controls: Vec<String> = (0..unsafe { part.GetControlInterfaceCount() }.unwrap_or(0))
        .filter_map(|i| unsafe { part.GetControlInterface(i) }.ok())
        .map(|c| {
            let iid = unsafe { c.GetIID() }
                .map(|g| format!("{g:?}"))
                .unwrap_or_default();
            let n = unsafe { c.GetName() }.map(take_string).unwrap_or_default();
            format!("{n} {iid}")
        })
        .collect();
    let _ = writeln!(
        out,
        "{indent}{kind} {name:?} local={local} subtype={sub} id={id}"
    );
    for c in controls {
        let _ = writeln!(out, "{indent}  control: {c}");
    }
    let next = unsafe {
        if incoming {
            part.EnumPartsIncoming()
        } else {
            part.EnumPartsOutgoing()
        }
    };
    if let Ok(list) = next {
        for i in 0..unsafe { list.GetCount() }.unwrap_or(0) {
            if let Ok(p) = unsafe { list.GetPart(i) } {
                dump_part(out, &p, depth + 1, incoming, seen);
            }
        }
    }
}

/// Every active endpoint and the parts around its adapter-side connector, both directions.
fn topology_dump() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let Ok(en) = enumerator() else {
        return "no audio device enumerator\n".into();
    };
    for (label, flow) in [("render", eRender), ("capture", eCapture)] {
        let Ok(list) = (unsafe { en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }) else {
            continue;
        };
        for i in 0..unsafe { list.GetCount() }.unwrap_or(0) {
            let Ok(device) = (unsafe { list.Item(i) }) else {
                continue;
            };
            let _ = writeln!(
                out,
                "== {label} endpoint {:?} adapter={:?} g6={}",
                friendly_name(&device).unwrap_or_default(),
                adapter_id(&device).unwrap_or_default(),
                is_g6(&device)
            );
            if let Some(part) = adapter_part(&device) {
                for (dir, incoming) in [
                    ("incoming (toward the hardware for capture)", true),
                    ("outgoing", false),
                ] {
                    let _ = writeln!(out, "  -- {dir}");
                    dump_part(&mut out, &part, 2, incoming, &mut Vec::new());
                }
            }
        }
    }
    out
}

// --- Backend ------------------------------------------------------------

pub struct WasapiMixer;

fn endpoint_for(scope: MixerScope) -> Result<IMMDevice, MixerError> {
    let flow = match scope {
        MixerScope::Playback => eRender,
        MixerScope::Recording => eCapture,
        MixerScope::Monitoring => {
            return Err(unavailable(
                "Windows has no hardware monitoring control for the G6",
            ))
        }
    };
    g6_endpoint(&enumerator()?, flow)
        .ok_or_else(|| unavailable("the G6 audio device was not found"))
}

impl MixerBackend for WasapiMixer {
    fn read(&self) -> Option<Mixer> {
        with_com(|| {
            let en = enumerator().ok()?;
            let render = g6_endpoint(&en, eRender);
            let capture = g6_endpoint(&en, eCapture);
            let playback = render.as_ref().and_then(level);
            let recording = capture.as_ref().and_then(level);
            if playback.is_none() && recording.is_none() {
                return None;
            }
            Some(Mixer {
                playback,
                monitoring: None,
                recording_source: capture.as_ref().and_then(sources),
                recording,
            })
        })
    }

    fn set_volume(&self, scope: MixerScope, volume: f32) -> Result<(), MixerError> {
        with_com(|| {
            let v = volume_of(&endpoint_for(scope)?)?;
            unsafe { v.SetMasterVolumeLevelScalar(volume, ptr::null()) }
                .map_err(|e| failed("set volume", e))
        })
    }

    fn set_mute(&self, scope: MixerScope, mute: bool) -> Result<(), MixerError> {
        with_com(|| {
            let v = volume_of(&endpoint_for(scope)?)?;
            unsafe { v.SetMute(mute, ptr::null()) }.map_err(|e| failed("set mute", e))
        })
    }

    fn set_source(&self, id: u32) -> Result<(), MixerError> {
        with_com(|| {
            let device = endpoint_for(MixerScope::Recording)?;
            let (selector, _) = selector_of(&device)
                .ok_or_else(|| unavailable("the G6 offers no recording source selector"))?;
            unsafe { selector.SetSelection(id, None) }.map_err(|e| failed("set source", e))
        })
    }

    fn subscribe(&self, notify: Notify) {
        // Temporary diagnostic: always written, in the temp folder and next to the program.
        let _ = std::thread::Builder::new()
            .name("mixer-dump".into())
            .spawn(|| {
                let text = std::panic::catch_unwind(|| with_com(topology_dump))
                    .unwrap_or_else(|_| "the topology dump panicked\n".into());
                let text = format!("opendac-control {}\n{text}", env!("CARGO_PKG_VERSION"));
                let name = "opendac-mixer-topology.txt";
                let mut dirs = vec![std::env::temp_dir()];
                if let Some(dir) = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                {
                    dirs.push(dir);
                }
                for dir in dirs {
                    let path = dir.join(name);
                    match std::fs::write(&path, &text) {
                        Ok(()) => log::info!("mixer: topology written to {}", path.display()),
                        Err(e) => log::warn!("mixer: cannot write {}: {e}", path.display()),
                    }
                }
            });
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::from(notify);
        let spawned = std::thread::Builder::new()
            .name("mixer-wasapi".into())
            .spawn(move || with_com(|| listen(notify)));
        if let Err(e) = spawned {
            log::warn!("mixer: cannot start the change listener: {e}");
        }
    }
}

// --- Change notifications -----------------------------------------------

enum Event {
    /// The set of devices (or the default one) changed: listen again.
    Devices,
    /// A volume or mute changed.
    Volume,
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeEvents(Sender<Event>);

impl IAudioEndpointVolumeCallback_Impl for VolumeEvents_Impl {
    fn OnNotify(&self, _: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        let _ = self.0.send(Event::Volume);
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceEvents(Sender<Event>);

impl IMMNotificationClient_Impl for DeviceEvents_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        let _ = self.0.send(Event::Devices);
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.0.send(Event::Devices);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        let _ = self.0.send(Event::Devices);
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        _: EDataFlow,
        _: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        let _ = self.0.send(Event::Devices);
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// An endpoint volume with our callback attached; detaches when dropped.
struct Watch {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = unsafe { self.volume.UnregisterControlChangeNotify(&self.callback) };
    }
}

fn watch_endpoints(en: &IMMDeviceEnumerator, tx: &Sender<Event>) -> Vec<Watch> {
    [eRender, eCapture]
        .into_iter()
        .filter_map(|flow| g6_endpoint(en, flow))
        .filter_map(|device| {
            let volume = volume_of(&device).ok()?;
            let callback: IAudioEndpointVolumeCallback = VolumeEvents(tx.clone()).into();
            unsafe { volume.RegisterControlChangeNotify(&callback) }.ok()?;
            Some(Watch { volume, callback })
        })
        .collect()
}

/// Body of the listener thread: calls `notify` on every volume, mute or device change.
fn listen(notify: Arc<dyn Fn() + Send + Sync>) {
    let en = match enumerator() {
        Ok(en) => en,
        Err(e) => {
            log::warn!("mixer: no change listener: {e}");
            return;
        }
    };
    let (tx, rx): (Sender<Event>, Receiver<Event>) = mpsc::channel();
    let client: IMMNotificationClient = DeviceEvents(tx.clone()).into();
    if let Err(e) = unsafe { en.RegisterEndpointNotificationCallback(&client) } {
        log::warn!("mixer: cannot watch for device changes: {e}");
    }
    // Registered and unregistered here, never from inside a callback.
    let mut watches = watch_endpoints(&en, &tx);
    while let Ok(event) = rx.recv() {
        if matches!(event, Event::Devices) {
            watches = watch_endpoints(&en, &tx);
        }
        notify();
    }
    drop(watches);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g6_is_found_by_usb_id_in_any_case() {
        assert!(is_g6_id(
            r"{2}.\\?\usb#VID_041E&PID_3256&MI_00#8&1a2b3c#{6994ad04-93ef-11d0-a3cc-00a0c9223196}"
        ));
        assert!(!is_g6_id(r"{2}.\\?\usb#vid_041e&pid_3257&mi_00#8&1a2b3c"));
        assert!(!is_g6_id(""));
    }

    #[test]
    fn g6_is_found_by_friendly_name() {
        assert!(is_g6_name("Speakers (Sound BlasterX G6)"));
        assert!(is_g6_name("Microphone (SBX G6)"));
        assert!(!is_g6_name("Speakers (Realtek High Definition Audio)"));
    }

    #[test]
    fn monitoring_is_not_offered() {
        let m = WasapiMixer;
        let e = m.set_volume(MixerScope::Monitoring, 0.5).unwrap_err();
        assert!(matches!(e, MixerError::Unavailable(_)));
    }
}
