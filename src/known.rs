//! Remembers receivers' names in `~/.airdrop/known.json`, so a receiver that later switches to
//! Contacts Only (and withholds its name) can still be recognised.
//!
//! A receiver's host changed on every AirDrop mode switch in testing, and its Bonjour ID on every
//! switch to Everyone, but its link-local AWDL address stayed the same for hours, across mode
//! switches. Records are
//! matched on any of the three, and learn the new ones each time they match.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::discovery::Peer;

/// How many of each identifier a record keeps, most recent first.
const KEEP: usize = 8;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Bonjour IDs, most recent first.
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Link-local IPv6 addresses, without their `%interface` scope.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// When the name was last reported by the receiver or set by hand (Unix seconds).
    pub named_at: u64,
    /// When the receiver was last seen (Unix seconds).
    pub seen_at: u64,
}

/// A remembered name for a receiver that did not report one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Known {
    pub name: String,
    pub model: Option<String>,
    /// The identifier that matched: `id`, `host` or `address`.
    pub matched_by: &'static str,
    /// When the name was learned (Unix seconds).
    pub named_at: u64,
}

/// The identifiers a receiver was seen with.
#[derive(Debug, Clone, Default)]
pub struct Sighting {
    pub id: String,
    pub host: Option<String>,
    pub addresses: Vec<String>,
}

impl Sighting {
    pub fn of(p: &Peer) -> Self {
        Sighting {
            id: p.id.clone(),
            host: p.network.host.clone(),
            addresses: p
                .network
                .addresses
                .iter()
                .filter(|a| a.to_ascii_lowercase().starts_with("fe80:"))
                .map(|a| a.split('%').next().unwrap_or(a).to_owned())
                .collect(),
        }
    }
}

/// Moves `value` to the front of `list`, keeping at most `KEEP`.
fn push_front(list: &mut Vec<String>, value: &str) {
    list.retain(|v| v != value);
    list.insert(0, value.to_owned());
    list.truncate(KEEP);
}

impl Store {
    /// The record `s` matches, and on which identifier.
    fn find(&self, s: &Sighting) -> Option<(usize, &'static str)> {
        let by = |f: &dyn Fn(&Device) -> bool| self.devices.iter().position(f);
        by(&|d| d.ids.contains(&s.id))
            .map(|i| (i, "id"))
            .or_else(|| {
                let h = s.host.as_ref()?;
                by(&|d| d.hosts.contains(h)).map(|i| (i, "host"))
            })
            .or_else(|| {
                by(&|d| s.addresses.iter().any(|a| d.addresses.contains(a))).map(|i| (i, "address"))
            })
    }

    /// Adds `s`'s identifiers to record `i`, taking them away from any other record.
    fn learn(&mut self, i: usize, s: &Sighting, now: u64) {
        for (j, d) in self.devices.iter_mut().enumerate() {
            if j == i {
                push_front(&mut d.ids, &s.id);
                if let Some(h) = &s.host {
                    push_front(&mut d.hosts, h);
                }
                for a in s.addresses.iter().rev() {
                    push_front(&mut d.addresses, a);
                }
                d.seen_at = now;
            } else {
                d.ids.retain(|v| *v != s.id);
                d.hosts.retain(|v| Some(v) != s.host.as_ref());
                d.addresses.retain(|v| !s.addresses.contains(v));
            }
        }
    }

    /// Records that the receiver seen as `s` is called `name`.
    pub fn named(&mut self, s: &Sighting, name: &str, model: Option<&str>, now: u64) {
        let i = match self
            .find(s)
            .map(|(i, _)| i)
            .or_else(|| self.devices.iter().position(|d| d.name == name))
        {
            Some(i) => i,
            None => {
                self.devices.push(Device {
                    name: String::new(),
                    model: None,
                    ids: Vec::new(),
                    hosts: Vec::new(),
                    addresses: Vec::new(),
                    named_at: now,
                    seen_at: now,
                });
                self.devices.len() - 1
            }
        };
        let d = &mut self.devices[i];
        d.name = name.to_owned();
        if let Some(m) = model {
            d.model = Some(m.to_owned());
        }
        d.named_at = now;
        self.learn(i, s, now);
    }

    /// The remembered name for the receiver seen as `s`, if any.
    pub fn recognise(&mut self, s: &Sighting, now: u64) -> Option<Known> {
        let (i, matched_by) = self.find(s)?;
        self.learn(i, s, now);
        let d = &self.devices[i];
        Some(Known {
            name: d.name.clone(),
            model: d.model.clone(),
            matched_by,
            named_at: d.named_at,
        })
    }

    /// Removes the records called `key` or seen with ID `key`, returning how many.
    pub fn forget(&mut self, key: &str) -> usize {
        let before = self.devices.len();
        self.devices
            .retain(|d| d.name != key && !d.ids.iter().any(|i| i == key));
        before - self.devices.len()
    }

    pub fn path() -> Result<PathBuf, String> {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(PathBuf::from(home).join(".airdrop").join("known.json"))
    }

    /// Reads the store; a missing file is an empty store.
    pub fn load() -> Result<Self, String> {
        let path = Self::path()?;
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("cannot parse {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// Writes the store, replacing the file atomically.
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        let err = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body).map_err(err)?;
        std::fs::rename(&tmp, &path).map_err(err)
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Remembers the names receivers report, and fills in `known` for the ones that withheld it.
pub fn apply(peers: &mut [Peer]) -> Result<(), String> {
    if peers.is_empty() {
        return Ok(());
    }
    let mut store = Store::load()?;
    let before = store.clone();
    let now = now();
    for p in peers.iter_mut() {
        let s = Sighting::of(p);
        match &p.name {
            Some(name) => store.named(&s, name, p.model.as_deref(), now),
            None => p.known = store.recognise(&s, now),
        }
    }
    if store == before {
        return Ok(());
    }
    store.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(id: &str, host: &str, address: &str) -> Sighting {
        Sighting {
            id: id.into(),
            host: Some(host.into()),
            addresses: vec![address.into()],
        }
    }

    #[test]
    fn recognised_after_mode_switch_by_address() {
        let mut s = Store::default();
        s.named(
            &seen("5de4dfbf8871", "319ff622", "fe80::8c73"),
            "k’s iPhone",
            None,
            1,
        );
        // Contacts Only: new ID and host, same AWDL address.
        let k = s
            .recognise(&seen("6c59f07c44ae", "e1941e4a", "fe80::8c73"), 2)
            .unwrap();
        assert_eq!(
            (k.name.as_str(), k.matched_by, k.named_at),
            ("k’s iPhone", "address", 1)
        );
        // The new ID was learned, so it matches even after the address rotates.
        let k = s
            .recognise(&seen("6c59f07c44ae", "x", "fe80::1"), 3)
            .unwrap();
        assert_eq!(k.matched_by, "id");
        assert_eq!(s.devices.len(), 1);
        assert_eq!(s.devices[0].seen_at, 3);
    }

    #[test]
    fn unknown_stays_unknown() {
        let mut s = Store::default();
        s.named(&seen("a", "h1", "fe80::1"), "Mac", Some("MacBook Air"), 1);
        assert_eq!(s.recognise(&seen("b", "h2", "fe80::2"), 2), None);
    }

    #[test]
    fn rename_and_identifier_moves() {
        let mut s = Store::default();
        s.named(&seen("a", "h1", "fe80::1"), "Old", None, 1);
        s.named(&seen("b", "h1", "fe80::2"), "New", None, 2);
        assert_eq!(s.devices.len(), 1);
        assert_eq!(s.devices[0].name, "New");
        s.named(&seen("c", "h3", "fe80::3"), "Other", None, 3);
        assert_eq!(s.devices.len(), 2);
        // An address now seen on the other device leaves the first record.
        s.named(&seen("c", "h3", "fe80::1"), "Other", None, 4);
        assert_eq!(s.devices.len(), 2);
        assert!(!s.devices[0].addresses.contains(&"fe80::1".to_owned()));
        assert_eq!(s.devices[1].addresses, ["fe80::1", "fe80::3"]);
    }

    #[test]
    fn forget_by_name_or_id() {
        let mut s = Store::default();
        s.named(&seen("a", "h1", "fe80::1"), "Mac", None, 1);
        s.named(&seen("b", "h2", "fe80::2"), "Phone", None, 1);
        assert_eq!(s.forget("a"), 1);
        assert_eq!(s.forget("Phone"), 1);
        assert_eq!(s.forget("nothing"), 0);
        assert!(s.devices.is_empty());
    }

    #[test]
    fn keeps_recent_identifiers() {
        let mut s = Store::default();
        for i in 0..20 {
            s.named(&seen(&format!("id{i}"), "h", "fe80::1"), "Mac", None, i);
        }
        assert_eq!(s.devices[0].ids.len(), KEEP);
        assert_eq!(s.devices[0].ids[0], "id19");
    }
}
