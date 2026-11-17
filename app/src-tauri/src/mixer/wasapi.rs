//! Core Audio (WASAPI) backend of the mixer (Windows).
//!
//! Windows presents the G6's USB audio function as audio *endpoints*: one or
//! more render endpoints (playback) and capture endpoints (recording). The
//! endpoint's `IAudioEndpointVolume` is the master volume and mute that the
//! Sound control panel shows; the class driver maps it to the USB feature
//! unit on the endpoint's signal path. That gives playback and recording.
//!
//! With the Creative driver the G6 shows one capture endpoint per input (Line In, External Mic,
//! SPDIF In, What U Hear) and one shared input selector. The recording source is that selector, and
//! recording is the selected input's endpoint. Monitoring is the selected input's own volume and mute
//! on its way to the speakers (IAudioVolumeLevel / IAudioMute on the parts of the signal path); What
//! U Hear has none. Monitoring changes made elsewhere are not announced (parts have no endpoint
//! callbacks here), they show on the next read.
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
    eCapture, eConsole, eRender, EDataFlow, ERole, IAudioInputSelector, IAudioMute,
    IAudioVolumeLevel, IConnector, IDeviceTopology, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, IPart, MMDeviceEnumerator, Subunit,
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE,
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

/// All of the G6's active endpoints for `flow`.
fn g6_endpoints(en: &IMMDeviceEnumerator, flow: EDataFlow) -> Vec<IMMDevice> {
    let Ok(list) = (unsafe { en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }) else {
        return Vec::new();
    };
    let count = unsafe { list.GetCount() }.unwrap_or(0);
    (0..count)
        .filter_map(|i| unsafe { list.Item(i) }.ok())
        .filter(is_g6)
        .collect()
}

/// The G6's endpoint for `flow`: the system default one when that is a G6, else the first.
fn g6_endpoint(en: &IMMDeviceEnumerator, flow: EDataFlow) -> Option<IMMDevice> {
    let found = g6_endpoints(en, flow);
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

// --- Topology: recording sources and monitoring ------------------------
//
// Seen on a G6 with the Creative driver (docs/protocol-g6.md, "Mixer"): each input (Line In,
// External Mic, SPDIF In, What U Hear) is its own capture endpoint. Its path runs from the
// input's connector through an ADC, a recording volume and a mute into one shared input selector
// (the "Mux"), which feeds the host. Line In, External Mic and SPDIF In also have a second path,
// through their own volume and mute into the "Sum" in front of the speakers: that is monitoring.

/// The connector of an endpoint's signal path on the adapter side, as a part.
fn adapter_part(device: &IMMDevice) -> Option<IPart> {
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    let own: IConnector = unsafe { topology.GetConnector(0) }.ok()?;
    let adapter_side: IConnector = unsafe { own.GetConnectedTo() }.ok()?;
    adapter_side.cast().ok()
}

/// A control interface of a part (volume, mute, selector), if it has it.
fn activate<T: Interface>(part: &IPart) -> Option<T> {
    let mut raw: *mut c_void = ptr::null_mut();
    unsafe { part.Activate(CLSCTX_ALL.0, &T::IID, Some(&mut raw)) }.ok()?;
    (!raw.is_null()).then(|| unsafe { T::from_raw(raw) })
}

/// The parts one step away from `part`.
fn neighbours(part: &IPart, incoming: bool) -> Vec<IPart> {
    let list = unsafe {
        if incoming {
            part.EnumPartsIncoming()
        } else {
            part.EnumPartsOutgoing()
        }
    };
    let Ok(list) = list else {
        return Vec::new();
    };
    (0..unsafe { list.GetCount() }.unwrap_or(0))
        .filter_map(|i| unsafe { list.GetPart(i) }.ok())
        .collect()
}

/// The input selector downstream of `part`, and the local id of the part that feeds it on this path.
fn find_mux(part: &IPart, depth: u32) -> Option<(IAudioInputSelector, u32)> {
    if depth > MAX_TOPOLOGY_DEPTH {
        return None;
    }
    for child in neighbours(part, false) {
        if unsafe { child.GetPartType() }.is_ok_and(|t| t == Subunit) {
            if let Some(selector) = activate::<IAudioInputSelector>(&child) {
                return Some((selector, unsafe { part.GetLocalId() }.ok()?));
            }
        }
        if let Some(found) = find_mux(&child, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// One recording source: its capture endpoint and the id the selector knows it by.
struct Source {
    id: u32,
    name: String,
    device: IMMDevice,
    /// The input's connector on the adapter side.
    connector: IPart,
}

/// The G6's recording sources and the selector that chooses between them.
struct Sources {
    selector: IAudioInputSelector,
    list: Vec<Source>,
    /// The id the selector reports as selected.
    current: Option<u32>,
}

impl Sources {
    fn find(en: &IMMDeviceEnumerator) -> Option<Sources> {
        let mut selector = None;
        let mut list = Vec::new();
        for device in g6_endpoints(en, eCapture) {
            let Some(connector) = adapter_part(&device) else {
                continue;
            };
            let Some((sel, id)) = find_mux(&connector, 0) else {
                continue;
            };
            let Ok(name) = (unsafe { connector.GetName() }) else {
                continue;
            };
            selector.get_or_insert(sel);
            list.push(Source {
                id,
                name: take_string(name),
                device,
                connector,
            });
        }
        let selector = selector?;
        let current = unsafe { selector.GetSelection() }.ok();
        (!list.is_empty()).then_some(Sources {
            selector,
            list,
            current,
        })
    }

    fn selected(&self) -> Option<&Source> {
        self.list.iter().find(|s| Some(s.id) == self.current)
    }

    fn wire(&self) -> Option<MixerSources> {
        Some(MixerSources {
            current: self.current?,
            options: self
                .list
                .iter()
                .map(|s| MixerSource {
                    id: s.id,
                    name: s.name.clone(),
                })
                .collect(),
        })
    }
}

/// An input's level on its way to the speakers: a volume (in dB) and a mute part.
struct Monitor {
    volume: IAudioVolumeLevel,
    mute: Option<IAudioMute>,
}

/// Scalar 0..1 for a level in dB, linear in dB over the part's range.
fn db_to_scalar(db: f32, min: f32, max: f32) -> f32 {
    if max > min {
        ((db - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn scalar_to_db(scalar: f32, min: f32, max: f32) -> f32 {
    min + scalar.clamp(0.0, 1.0) * (max - min)
}

impl Monitor {
    /// The first volume and mute on the path from the input's connector to the speakers' connector.
    fn find(source: &Source, render: &IMMDevice) -> Option<Monitor> {
        let to = adapter_part(render)?;
        let topology = unsafe { source.connector.GetTopologyObject() }.ok()?;
        let path = unsafe { topology.GetSignalPath(&source.connector, &to, false) }.ok()?;
        let (mut volume, mut mute) = (None, None);
        for i in 0..unsafe { path.GetCount() }.unwrap_or(0) {
            let Ok(part) = (unsafe { path.GetPart(i) }) else {
                continue;
            };
            volume = volume.or_else(|| activate::<IAudioVolumeLevel>(&part));
            mute = mute.or_else(|| activate::<IAudioMute>(&part));
        }
        Some(Monitor {
            volume: volume?,
            mute,
        })
    }

    fn range(&self) -> Option<(f32, f32)> {
        let (mut min, mut max, mut step) = (0.0, 0.0, 0.0);
        unsafe { self.volume.GetLevelRange(0, &mut min, &mut max, &mut step) }.ok()?;
        (max > min).then_some((min, max))
    }

    fn level(&self) -> Option<MixerLevel> {
        let (min, max) = self.range()?;
        let db = unsafe { self.volume.GetLevel(0) }.ok()?;
        Some(MixerLevel {
            volume: db_to_scalar(db, min, max),
            volume_db: Some(db),
            mute: self
                .mute
                .as_ref()
                .and_then(|m| unsafe { m.GetMute() }.ok())
                .map(|m| m.as_bool()),
        })
    }

    fn set_volume(&self, scalar: f32) -> Result<(), MixerError> {
        let (min, max) = self
            .range()
            .ok_or_else(|| unavailable("the monitoring level has no range"))?;
        unsafe {
            self.volume
                .SetLevelUniform(scalar_to_db(scalar, min, max), None)
        }
        .map_err(|e| failed("set monitoring level", e))
    }

    fn set_mute(&self, mute: bool) -> Result<(), MixerError> {
        let m = self
            .mute
            .as_ref()
            .ok_or_else(|| unavailable("the monitoring level has no mute"))?;
        unsafe { m.SetMute(mute, None) }.map_err(|e| failed("set monitoring mute", e))
    }
}

// --- Diagnostic dump ----------------------------------------------------

/// Set this environment variable to write the audio topology at start-up.
const DUMP_VAR: &str = "OPENDAC_MIXER_DUMP";

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

/// What the G6 offers right now, looked up afresh for every call.
struct Ctx {
    render: Option<IMMDevice>,
    sources: Option<Sources>,
    /// Capture endpoint of the selected source, else the system default one.
    capture: Option<IMMDevice>,
}

impl Ctx {
    fn new() -> Result<Ctx, MixerError> {
        let en = enumerator()?;
        let sources = Sources::find(&en);
        let capture = sources
            .as_ref()
            .and_then(Sources::selected)
            .map(|s| s.device.clone())
            .or_else(|| g6_endpoint(&en, eCapture));
        Ok(Ctx {
            render: g6_endpoint(&en, eRender),
            sources,
            capture,
        })
    }

    fn monitor(&self) -> Option<Monitor> {
        let source = self.sources.as_ref()?.selected()?;
        Monitor::find(source, self.render.as_ref()?)
    }

    /// The endpoint whose master volume and mute `scope` is.
    fn endpoint(&self, scope: MixerScope) -> Result<&IMMDevice, MixerError> {
        let device = match scope {
            MixerScope::Playback => &self.render,
            MixerScope::Recording => &self.capture,
            MixerScope::Monitoring => &None,
        };
        device
            .as_ref()
            .ok_or_else(|| unavailable("the G6 audio device was not found"))
    }
}

impl MixerBackend for WasapiMixer {
    fn read(&self) -> Option<Mixer> {
        with_com(|| {
            let ctx = Ctx::new().ok()?;
            let playback = ctx.render.as_ref().and_then(level);
            let recording = ctx.capture.as_ref().and_then(level);
            if playback.is_none() && recording.is_none() {
                return None;
            }
            Some(Mixer {
                playback,
                monitoring: ctx.monitor().and_then(|m| m.level()),
                recording_source: ctx.sources.as_ref().and_then(Sources::wire),
                recording,
            })
        })
    }

    fn set_volume(&self, scope: MixerScope, volume: f32) -> Result<(), MixerError> {
        with_com(|| {
            let ctx = Ctx::new()?;
            if scope == MixerScope::Monitoring {
                let m = ctx
                    .monitor()
                    .ok_or_else(|| unavailable("this input has no monitoring control"))?;
                return m.set_volume(volume);
            }
            let v = volume_of(ctx.endpoint(scope)?)?;
            unsafe { v.SetMasterVolumeLevelScalar(volume, ptr::null()) }
                .map_err(|e| failed("set volume", e))
        })
    }

    fn set_mute(&self, scope: MixerScope, mute: bool) -> Result<(), MixerError> {
        with_com(|| {
            let ctx = Ctx::new()?;
            if scope == MixerScope::Monitoring {
                let m = ctx
                    .monitor()
                    .ok_or_else(|| unavailable("this input has no monitoring control"))?;
                return m.set_mute(mute);
            }
            let v = volume_of(ctx.endpoint(scope)?)?;
            unsafe { v.SetMute(mute, ptr::null()) }.map_err(|e| failed("set mute", e))
        })
    }

    fn set_source(&self, id: u32) -> Result<(), MixerError> {
        with_com(|| {
            let sources = Sources::find(&enumerator()?)
                .ok_or_else(|| unavailable("the G6 offers no recording source selector"))?;
            if !sources.list.iter().any(|s| s.id == id) {
                return Err(unavailable("unknown recording source"));
            }
            unsafe { sources.selector.SetSelection(id, None) }.map_err(|e| failed("set source", e))
        })
    }

    fn subscribe(&self, notify: Notify) {
        // Diagnostic: with OPENDAC_MIXER_DUMP set, write the audio topology next to the program and
        // to the temp folder (what Windows shows for the G6).
        if std::env::var_os(DUMP_VAR).is_some() {
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
        }
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
        .flat_map(|flow| g6_endpoints(en, flow))
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
    fn level_conversion_is_linear_in_db_and_clamped() {
        assert_eq!(db_to_scalar(-48.0, -48.0, 9.0), 0.0);
        assert_eq!(db_to_scalar(9.0, -48.0, 9.0), 1.0);
        assert_eq!(db_to_scalar(-19.5, -48.0, 9.0), 0.5);
        assert_eq!(db_to_scalar(20.0, -48.0, 9.0), 1.0);
        assert_eq!(db_to_scalar(0.0, 0.0, 0.0), 0.0);
        assert_eq!(scalar_to_db(0.5, -48.0, 9.0), -19.5);
        assert_eq!(scalar_to_db(2.0, -48.0, 9.0), 9.0);
    }
}
