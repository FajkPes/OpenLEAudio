//! Devices we have already paired with.
//!
//! This is what lets the device list say "Paired" and offer *Connect*
//! instead of starting pairing from scratch every time. Without it every
//! connection is a first connection, which on LE Audio means a fresh key
//! exchange and a device that may well refuse because it already believes it
//! knows us.
//!
//! **This file contains long term keys.** They are the shared secret that
//! encrypts the link, so the store lives under the user's own profile and
//! nowhere else. Windows keeps the equivalent in a registry hive only SYSTEM
//! can read; we cannot write there without a service, so the honest position is
//! that this file is exactly as sensitive as the user's profile directory. The
//! worst a leaked key allows is impersonating this PC to those headphones - it
//! is not a credential for anything else - but it is still not something to
//! copy around or paste into a bug report.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One remembered device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bond {
    /// Public address, in display form.
    pub address: String,
    pub name: String,
    /// The long term key, as agreed during pairing.
    pub long_term_key: [u8; 16],
    /// Whether the device advertised LE Audio, so the list can say so.
    pub le_audio: bool,
}

impl Bond {
    /// A one-line summary that never includes the key.
    pub fn describe(&self) -> String {
        let kind = if self.le_audio { "LE Audio" } else { "LE" };
        format!("{} ({kind}, {})", self.name, self.address)
    }
}

/// Every device this stack has paired with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BondStore {
    bonds: BTreeMap<String, Bond>,
}

impl BondStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the store lives: beside the user's other settings.
    pub fn default_path() -> PathBuf {
        let base = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        base.join("OpenLEAudio").join("bonds.txt")
    }

    pub fn get(&self, address: &str) -> Option<&Bond> {
        self.bonds.get(&address.to_ascii_uppercase())
    }

    pub fn contains(&self, address: &str) -> bool {
        self.get(address).is_some()
    }

    /// Remembers a device, replacing any earlier key for the same address.
    ///
    /// Replacing rather than keeping both is deliberate: a second pairing means
    /// the old key is dead, and holding on to it only makes a later connection
    /// fail in a way nobody can explain.
    pub fn insert(&mut self, bond: Bond) {
        self.bonds.insert(bond.address.to_ascii_uppercase(), bond);
    }

    /// Forgets a device, as the "Unpair" button does.
    pub fn remove(&mut self, address: &str) -> bool {
        self.bonds.remove(&address.to_ascii_uppercase()).is_some()
    }

    pub fn all(&self) -> impl Iterator<Item = &Bond> {
        self.bonds.values()
    }

    pub fn len(&self) -> usize {
        self.bonds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bonds.is_empty()
    }

    fn to_text(&self) -> String {
        let mut text = String::from(
            "# OpenLEAudio paired devices\n\
             # WARNING: this file contains encryption keys. Do not share it.\n\
             # address | name | LE Audio | key\n\n",
        );

        for bond in self.bonds.values() {
            let key: String = bond.long_term_key.iter().map(|b| format!("{b:02X}")).collect();
            let name = bond.name.replace('|', "/");
            text.push_str(&format!(
                "{} | {} | {} | {}\n",
                bond.address, name, bond.le_audio, key
            ));
        }

        text
    }

    fn from_text(text: &str) -> Self {
        let mut store = Self::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let fields: Vec<&str> = line.split('|').map(|f| f.trim()).collect();
            let [address, name, le_audio, key] = fields.as_slice() else {
                continue;
            };

            let Some(long_term_key) = parse_key(key) else {
                continue;
            };

            store.insert(Bond {
                address: address.to_string(),
                name: name.to_string(),
                le_audio: *le_audio == "true",
                long_term_key,
            });
        }

        store
    }

    /// Loads the store, treating a missing or damaged file as "nothing paired yet".
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::from_text(&text))
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_text())
    }
}

fn parse_key(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }

    let mut key = [0u8; 16];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bond(address: &str, name: &str) -> Bond {
        Bond {
            address: address.into(),
            name: name.into(),
            long_term_key: [0xAB; 16],
            le_audio: true,
        }
    }

    #[test]
    fn a_saved_store_reads_back_the_same() {
        let mut store = BondStore::new();
        store.insert(bond("7C:FE:62:72:B4:9A", "JBL Tune 780NC"));
        store.insert(bond("AA:BB:CC:DD:EE:FF", "JBL Tune 720BT"));

        assert_eq!(BondStore::from_text(&store.to_text()), store);
    }

    #[test]
    fn address_case_does_not_create_a_second_entry() {
        let mut store = BondStore::new();
        store.insert(bond("7c:fe:62:72:b4:9a", "JBL Tune 780NC"));

        assert!(store.contains("7C:FE:62:72:B4:9A"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn pairing_again_replaces_the_dead_key() {
        let mut store = BondStore::new();
        store.insert(bond("7C:FE:62:72:B4:9A", "JBL Tune 780NC"));

        let mut fresh = bond("7C:FE:62:72:B4:9A", "JBL Tune 780NC");
        fresh.long_term_key = [0x11; 16];
        store.insert(fresh);

        assert_eq!(store.len(), 1);
        assert_eq!(store.get("7C:FE:62:72:B4:9A").unwrap().long_term_key, [0x11; 16]);
    }

    #[test]
    fn a_device_can_be_forgotten() {
        let mut store = BondStore::new();
        store.insert(bond("7C:FE:62:72:B4:9A", "JBL Tune 780NC"));

        assert!(store.remove("7c:fe:62:72:b4:9a"));
        assert!(!store.remove("7C:FE:62:72:B4:9A"));
        assert!(store.is_empty());
    }

    #[test]
    fn a_damaged_line_costs_one_device_not_the_file() {
        let store = BondStore::from_text(
            "7C:FE:62:72:B4:9A | JBL | true | ABABABABABABABABABABABABABABABAB\n\
             AA:BB:CC:DD:EE:FF | Broken | true | nonsense\n",
        );

        assert_eq!(store.len(), 1);
        assert!(store.contains("7C:FE:62:72:B4:9A"));
    }

    #[test]
    fn a_missing_file_means_nothing_is_paired_yet() {
        let store = BondStore::load(Path::new("this-file-does-not-exist.txt"));
        assert!(store.is_empty());
    }

    #[test]
    fn the_summary_never_contains_the_key() {
        let summary = bond("7C:FE:62:72:B4:9A", "JBL Tune 780NC").describe();

        assert!(summary.contains("JBL Tune 780NC"));
        assert!(!summary.to_ascii_uppercase().contains("ABAB"));
    }
}
