//! Lists Apple devices by the Continuity advertisements they send over Bluetooth LE.
//!
//! Listens only: Apple devices advertise unprompted, so this sees devices with Bluetooth on
//! whether or not AirDrop or AWDL is. Message formats follow furiousMAC's Continuity reverse
//! engineering (github.com/furiousMAC/continuity), done on iOS 13, so labels may be off on newer
//! systems; every message is also kept as raw bytes.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_core_bluetooth::{
    CBAdvertisementDataLocalNameKey, CBAdvertisementDataManufacturerDataKey, CBCentralManager,
    CBCentralManagerDelegate, CBCentralManagerScanOptionAllowDuplicatesKey, CBManagerState,
    CBPeripheral,
};
use objc2_foundation::{NSData, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use serde::Serialize;

/// Bluetooth SIG company identifier for Apple, little-endian.
const APPLE: [u8; 2] = [0x4c, 0x00];

/// An Apple device heard advertising.
#[derive(Debug, Clone, Serialize)]
pub struct BleDevice {
    /// CoreBluetooth's identifier for the advertiser. Only meaningful on this Mac, and it
    /// changes when the device rotates its random Bluetooth address (about every 15 minutes).
    pub id: String,
    /// Advertised local name. Apple devices rarely send one.
    pub name: Option<String>,
    /// Strongest signal seen, in dBm. Closer to 0 is nearer.
    pub rssi: i32,
    /// Best guess at what the device is, from the message types it sends.
    pub kind: Option<&'static str>,
    /// The Nearby Info "AirDrop receiving" bit: set unless AirDrop is Receiving Off. `None`
    /// when the device sent no Nearby Info.
    pub airdrop: Option<bool>,
    /// Latest message of each type.
    pub messages: Vec<Message>,
}

/// One Continuity message from Apple's manufacturer data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    #[serde(rename = "type")]
    pub kind: u8,
    pub name: Option<&'static str>,
    /// Decoded fields, as short phrases.
    pub details: Vec<String>,
    /// Decoded fields whose meaning comes from pre-2021 research and has not been confirmed
    /// on current devices (e.g. Nearby Info activity, which labels a phone on a desk as
    /// "driving").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
    pub hex: String,
    #[serde(skip)]
    pub payload: Vec<u8>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn type_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        0x02 => "iBeacon",
        0x03 => "AirPrint",
        0x05 => "AirDrop",
        0x06 => "HomeKit",
        0x07 => "proximity pairing",
        0x08 => "Hey Siri",
        0x09 => "AirPlay target",
        0x0a => "AirPlay source",
        0x0b => "Magic Switch",
        0x0c => "Handoff",
        0x0d => "tethering target",
        0x0e => "tethering source",
        0x0f => "nearby action",
        0x10 => "nearby info",
        0x12 => "Find My",
        _ => return None,
    })
}

/// Nearby Info action codes, the low nibble of the first byte.
fn activity(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "activity unknown",
        0x01 => "activity reporting disabled",
        0x03 => "locked",
        0x05 => "audio playing, screen off",
        0x07 => "screen turning on or off",
        0x09 => "video playing",
        0x0a => "locked, notifications to Watch",
        0x0b => "active",
        0x0d => "driving",
        0x0e => "on a call",
        _ => return None,
    })
}

fn airpods_model(model: u16) -> Option<&'static str> {
    Some(match model {
        0x0220 => "AirPods 1",
        0x0f20 => "AirPods 2",
        0x0e20 => "AirPods Pro",
        0x0320 => "Powerbeats3",
        0x0520 => "BeatsX",
        0x0620 => "Beats Solo3",
        _ => return None,
    })
}

impl Message {
    pub fn decode(kind: u8, payload: &[u8]) -> Self {
        let mut details = Vec::new();
        let mut unverified = Vec::new();
        match (kind, payload) {
            (0x10, [status, rest @ ..]) => {
                // Confirmed on a current iPhone: only this bit flips when AirDrop is set to Receiving
                // Off, and it stays set in both Everyone and Contacts Only.
                let airdrop = if status & 0x40 != 0 { "on" } else { "off" };
                details.push(format!("AirDrop receiving {airdrop}"));
                let code = status & 0x0f;
                unverified.push(match activity(code) {
                    Some(a) => a.to_owned(),
                    None => format!("activity {code:#x}"),
                });
                if status & 0x10 != 0 {
                    unverified.push("primary iCloud device".into());
                }
                if let Some(data) = rest.first() {
                    let wifi = if data & 0x04 != 0 { "on" } else { "off" };
                    unverified.push(format!("Wi-Fi {wifi}"));
                }
            }
            // Other prefixes use a different, undocumented layout.
            (0x07, [0x01, m0, m1, _status, battery, charging, ..]) => {
                let model = u16::from_be_bytes([*m0, *m1]);
                details.push(match airpods_model(model) {
                    Some(m) => m.to_owned(),
                    None => format!("model {model:#06x}"),
                });
                // Levels are tens of percent; 0xf means unknown.
                let level = |v: u8| (v <= 10).then(|| format!("{}%", v * 10));
                let parts: Vec<String> = [
                    ("left", battery & 0x0f),
                    ("right", battery >> 4),
                    ("case", charging & 0x0f),
                ]
                .into_iter()
                .filter_map(|(what, v)| level(v).map(|l| format!("{what} {l}")))
                .collect();
                if !parts.is_empty() {
                    details.push(format!("battery {}", parts.join(", ")));
                }
            }
            (0x12, [status, ..]) => {
                // Bit 2: the owner's device was connected within the current key period.
                if status & 0x04 != 0 {
                    let battery =
                        ["full", "medium", "low", "critically low"][usize::from(status >> 6)];
                    details.push(format!("owner nearby recently, battery {battery}"));
                } else {
                    details.push("separated from owner".into());
                }
            }
            _ => {}
        }
        Message {
            kind,
            name: type_name(kind),
            details,
            unverified,
            hex: hex(payload),
            payload: payload.to_vec(),
        }
    }
}

/// Splits Apple manufacturer data (company ID, then type-length-value messages). `None` when
/// it is not Apple's.
pub fn parse(data: &[u8]) -> Option<Vec<Message>> {
    let mut rest = data.strip_prefix(&APPLE)?;
    let mut messages = Vec::new();
    while let [kind, len, tail @ ..] = rest {
        let (payload, next) = tail.split_at(usize::from(*len).min(tail.len()));
        messages.push(Message::decode(*kind, payload));
        rest = next;
    }
    Some(messages)
}

/// Guesses the device from the message types it sends.
fn kind_of(messages: &[Message]) -> Option<&'static str> {
    let has = |k: u8| messages.iter().any(|m| m.kind == k);
    Some(if has(0x07) {
        "AirPods or Beats"
    } else if has(0x0b) {
        "Apple Watch"
    } else if has(0x10) {
        "iPhone, iPad, Mac or Watch"
    } else if has(0x09) {
        "AirPlay receiver"
    } else if has(0x06) {
        "HomeKit accessory"
    } else if has(0x12) {
        "Find My beacon (AirTag, or a device away from its owner)"
    } else if has(0x02) {
        "iBeacon"
    } else {
        return None;
    })
}

fn nearby_info(messages: &[Message]) -> Option<&[u8]> {
    messages
        .iter()
        .find(|m| m.kind == 0x10)
        .map(|m| m.payload.as_slice())
}

#[derive(Default)]
struct Ivars {
    devices: RefCell<BTreeMap<String, BleDevice>>,
    state: Cell<Option<CBManagerState>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and Scanner does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AirdropBleScanner"]
    #[ivars = Ivars]
    struct Scanner;

    unsafe impl NSObjectProtocol for Scanner {}

    unsafe impl CBCentralManagerDelegate for Scanner {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn did_update_state(&self, central: &CBCentralManager) {
            let state = unsafe { central.state() };
            self.ivars().state.set(Some(state));
            if state == CBManagerState::PoweredOn {
                // Without duplicates macOS reports each device once, hiding state changes.
                let on = NSNumber::new_bool(true);
                let options = NSDictionary::<NSString, AnyObject>::from_slices(
                    &[unsafe { CBCentralManagerScanOptionAllowDuplicatesKey }],
                    &[&*on],
                );
                unsafe { central.scanForPeripheralsWithServices_options(None, Some(&options)) };
            }
        }

        #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
        fn did_discover(
            &self,
            _central: &CBCentralManager,
            peripheral: &CBPeripheral,
            data: &NSDictionary<NSString, AnyObject>,
            rssi: &NSNumber,
        ) {
            let get = |key: &NSString| data.objectForKey(key);
            let Some(manufacturer) = get(unsafe { CBAdvertisementDataManufacturerDataKey })
                .and_then(|d| d.downcast::<NSData>().ok())
            else {
                return;
            };
            let Some(messages) = parse(&manufacturer.to_vec()) else {
                return;
            };
            let id = unsafe { peripheral.identifier() }.UUIDString().to_string();
            let name = get(unsafe { CBAdvertisementDataLocalNameKey })
                .and_then(|n| n.downcast::<NSString>().ok())
                .map(|n| n.to_string());
            // 127 means CoreBluetooth could not read the signal strength.
            let rssi = rssi.intValue();
            let mut devices = self.ivars().devices.borrow_mut();
            let device = devices.entry(id.clone()).or_insert_with(|| BleDevice {
                id,
                name: None,
                rssi: i32::MIN,
                kind: None,
                airdrop: None,
                messages: Vec::new(),
            });
            if name.is_some() {
                device.name = name;
            }
            if rssi != 127 {
                device.rssi = device.rssi.max(rssi);
            }
            for m in messages {
                match device.messages.iter_mut().find(|old| old.kind == m.kind) {
                    Some(old) => *old = m,
                    None => device.messages.push(m),
                }
            }
            device.messages.sort_by_key(|m| m.kind);
            device.kind = kind_of(&device.messages);
            device.airdrop = nearby_info(&device.messages)
                .and_then(<[u8]>::first)
                .map(|status| status & 0x40 != 0);
        }
    }
);

/// A running scan. Callbacks arrive on the main queue, so the caller must run the main run
/// loop between `start` and `finish`.
pub struct Scan {
    manager: Retained<CBCentralManager>,
    scanner: Retained<Scanner>,
}

impl Scan {
    pub fn start() -> Result<Self, String> {
        let mtm =
            MainThreadMarker::new().ok_or("the Bluetooth scan must run on the main thread")?;
        let scanner: Retained<Scanner> = {
            let this = Scanner::alloc(mtm).set_ivars(Ivars::default());
            unsafe { msg_send![super(this), init] }
        };
        // A plain init delivers events on the main queue.
        let manager: Retained<CBCentralManager> =
            unsafe { msg_send![CBCentralManager::alloc(), init] };
        unsafe { manager.setDelegate(Some(ProtocolObject::from_ref(&*scanner))) };
        Ok(Self { manager, scanner })
    }

    /// Stops scanning and returns the devices heard, nearest first.
    pub fn finish(self) -> Result<Vec<BleDevice>, String> {
        unsafe {
            self.manager.stopScan();
            self.manager.setDelegate(None);
        }
        let ivars = self.scanner.ivars();
        match ivars.state.get() {
            Some(CBManagerState::PoweredOn) => {}
            Some(CBManagerState::Unauthorized) => {
                return Err(
                    "Bluetooth access denied; allow your terminal app in System \
                    Settings > Privacy & Security > Bluetooth"
                        .into(),
                );
            }
            Some(CBManagerState::PoweredOff) => return Err("Bluetooth is off".into()),
            Some(CBManagerState::Unsupported) => {
                return Err("this Mac does not support Bluetooth LE".into());
            }
            _ => return Err("Bluetooth did not become ready in time".into()),
        }
        let mut devices: Vec<BleDevice> = ivars.devices.take().into_values().collect();
        devices.sort_by_key(|d| std::cmp::Reverse(d.rssi));
        // A device that rotates its address mid-scan shows up twice with the same Nearby Info.
        let mut seen: Vec<Vec<u8>> = Vec::new();
        devices.retain(|d| match nearby_info(&d.messages) {
            Some(p) if seen.iter().any(|s| s == p) => false,
            Some(p) => {
                seen.push(p.to_vec());
                true
            }
            None => true,
        });
        Ok(devices)
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, parse};

    #[test]
    fn not_apple() {
        assert!(parse(&[0x06, 0x00, 0x01, 0x09]).is_none());
    }

    #[test]
    fn nearby_info_and_handoff() {
        // Nearby Info: AirDrop on, active; Wi-Fi on. Then a Handoff message.
        let m = parse(&[
            0x4c, 0x00, 0x10, 0x05, 0x4b, 0x1c, 0x83, 0x90, 0x96, 0x0c, 0x03, 0x00, 0x5a, 0x48,
        ])
        .unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name, Some("nearby info"));
        assert_eq!(m[0].details, ["AirDrop receiving on"]);
        assert_eq!(m[0].unverified, ["active", "Wi-Fi on"]);
        assert_eq!(m[0].hex, "4b1c839096");
        assert_eq!(m[1].name, Some("Handoff"));
        assert_eq!(m[1].hex, "005a48");
    }

    #[test]
    fn airdrop_bit_on_an_iphone() {
        // Captured from one iPhone before and after setting AirDrop to Receiving Off.
        let on = Message::decode(0x10, &[0x75, 0x1c, 0xc8, 0x50, 0x8b]);
        let off = Message::decode(0x10, &[0x35, 0x1c, 0xc8, 0x50, 0x8b]);
        assert_eq!(on.details, ["AirDrop receiving on"]);
        assert_eq!(off.details, ["AirDrop receiving off"]);
        assert_eq!(on.unverified, off.unverified);
    }

    #[test]
    fn airpods() {
        let m = Message::decode(0x07, &[0x01, 0x0e, 0x20, 0x55, 0x89, 0x07, 0x39, 0x00]);
        assert_eq!(
            m.details,
            ["AirPods Pro", "battery left 90%, right 80%, case 70%"]
        );
    }

    #[test]
    fn airpods_other_layout() {
        let m = Message::decode(0x07, &[0x08, 0x09, 0x27, 0xdf, 0xbc, 0x27, 0x62]);
        assert!(m.details.is_empty());
        assert_eq!(m.hex, "080927dfbc2762");
    }

    #[test]
    fn find_my() {
        assert_eq!(
            Message::decode(0x12, &[0x64, 0x00]).details,
            ["owner nearby recently, battery medium"]
        );
        assert_eq!(
            Message::decode(0x12, &[0x00]).details,
            ["separated from owner"]
        );
    }

    #[test]
    fn truncated() {
        let m = parse(&[0x4c, 0x00, 0x10, 0x05, 0x1b]).unwrap();
        assert_eq!(m[0].hex, "1b");
        assert_eq!(m[0].details, ["AirDrop receiving off"]);
        assert_eq!(m[0].unverified, ["active", "primary iCloud device"]);
    }
}
