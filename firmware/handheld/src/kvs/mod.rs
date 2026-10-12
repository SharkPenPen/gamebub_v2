use embedded_svc::storage::RawStorage;
use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsCustom};
use serde::{de::DeserializeOwned, Serialize};
use smallvec::SmallVec;
use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};

pub mod keys;

static KVS: OnceLock<Mutex<Kvs>> = OnceLock::new();
static NAMESPACE: &'static str = "gb";

/// TODO: a few possible changes:
///  * Use a single global lock for all keys and all keys' caches?
///  * use a macro when defining keys to ensure they're all in flush_all
pub struct Kvs {
    /// Read-write settings. Missing means settings are not persisted.
    nvs_main: Option<EspNvs<NvsCustom>>,
    /// Read-only factory data (serial, hardware revision).
    nvs_ro: Option<EspNvs<NvsCustom>>,
}

impl Kvs {
    /// Open the storage namespaces.
    ///
    /// A namespace that is not there is **not** an error. The read-only
    /// partition holds per-device factory data that may never have been written
    /// (a hand-built board), and flashing a merged image erases it. Either way
    /// the firmware has to come up with defaults: a device that refuses to boot
    /// cannot tell anyone that its factory data is missing.
    pub fn init() -> Result<(), anyhow::Error> {
        let nvs_main = Self::open("nvs", true);
        let nvs_ro = Self::open("nvs_ro", false);

        let kvs = Kvs { nvs_main, nvs_ro };
        KVS.set(Mutex::new(kvs))
            .map_err(|_| ())
            .expect("KVS already initialized");

        Ok(())
    }

    /// Open one namespace, reporting rather than propagating a failure.
    fn open(label: &str, read_write: bool) -> Option<EspNvs<NvsCustom>> {
        let partition = match EspNvsPartition::<NvsCustom>::take(label) {
            Ok(partition) => partition,
            Err(error) => {
                log::warn!("[KVS] there is no '{label}' partition: {error}");
                return None;
            }
        };

        match EspNvs::new(partition, NAMESPACE, read_write) {
            Ok(nvs) => Some(nvs),
            Err(error) => {
                log::warn!(
                    "[KVS] the '{NAMESPACE}' namespace is not usable in '{label}' ({error}): defaults will be used"
                );
                None
            }
        }
    }

    pub fn get() -> MutexGuard<'static, Kvs> {
        KVS.get().unwrap().lock().unwrap()
    }

    fn nvs(&mut self, read_only: bool) -> Option<&mut EspNvs<NvsCustom>> {
        if read_only {
            self.nvs_ro.as_mut()
        } else {
            self.nvs_main.as_mut()
        }
    }
}

const SMALL_SIZE: usize = 128;

pub struct CacheEntry<T> {
    value: Option<T>,
    dirty: bool,
}

pub struct KvsKey<T> {
    name: &'static str,
    read_only: bool,
    default: Option<T>,
    cache: RwLock<Option<CacheEntry<T>>>,
}

impl<T: Serialize + DeserializeOwned + Clone> KvsKey<T> {
    const fn new(name: &'static str) -> Self {
        assert!(name.len() < 16);
        KvsKey::<T> {
            name,
            read_only: false,
            default: None,
            cache: RwLock::new(None),
        }
    }

    const fn new_with_default(name: &'static str, default: T) -> Self {
        assert!(name.len() < 16);
        KvsKey::<T> {
            name,
            read_only: false,
            default: Some(default),
            cache: RwLock::new(None),
        }
    }

    const fn new_ro(name: &'static str) -> Self {
        assert!(name.len() < 16);
        KvsKey::<T> {
            name,
            read_only: true,
            default: None,
            cache: RwLock::new(None),
        }
    }

    /// Get the value directly from NVS, without going through the cache.
    ///
    /// Every failure is reported and turned into "no value", so that a missing
    /// key, an erased partition or a value written by another firmware version
    /// falls back to the default instead of taking the application down.
    fn get_direct(&self) -> Option<T> {
        let mut kvs = Kvs::get();
        let nvs = kvs.nvs(self.read_only)?;

        let len = match nvs.len(self.name) {
            Ok(Some(len)) => len,
            Ok(None) => return None,
            Err(error) => {
                log::warn!("[KVS] cannot read the length of '{}': {error}", self.name);
                return None;
            }
        };

        let mut v = SmallVec::<[u8; SMALL_SIZE]>::from_elem(0, len);
        let data = match nvs.get_raw(self.name, &mut v) {
            Ok(Some(data)) => data,
            Ok(None) => return None,
            Err(error) => {
                log::warn!("[KVS] cannot read '{}': {error}", self.name);
                return None;
            }
        };

        match postcard::from_bytes(data) {
            Ok(value) => Some(value),
            Err(error) => {
                log::warn!(
                    "[KVS] '{}' holds a value this firmware cannot read: {error}",
                    self.name
                );
                None
            }
        }
    }

    pub fn get(&self) -> Option<T> {
        // If it's in the cache, return it.
        let cache = self.cache.read().unwrap();
        if let Some(value) = cache.as_ref() {
            return value.value.as_ref().or(self.default.as_ref()).cloned();
        }

        // Otherwise, fetch from storage.
        let value = self.get_direct();

        // Reload the cache.
        std::mem::drop(cache);
        let mut cache = self.cache.write().unwrap();
        *cache = Some(CacheEntry {
            value: value.clone(),
            dirty: false,
        });

        value.or_else(|| self.default.clone())
    }

    fn set_direct(&self, value: &T) {
        let mut kvs = Kvs::get();
        let Some(nvs) = kvs.nvs(self.read_only) else {
            log::warn!("[KVS] '{}' cannot be stored: no usable namespace", self.name);
            return;
        };

        let mut v = SmallVec::<[u8; SMALL_SIZE]>::new();
        if let Err(error) = postcard::to_io(value, &mut v) {
            log::warn!("[KVS] cannot serialize '{}': {error}", self.name);
            return;
        }

        if let Err(error) = nvs.set_raw(self.name, &v) {
            log::warn!("[KVS] cannot write '{}': {error}", self.name);
        }
    }

    pub fn set(&self, value: &T) {
        let mut cache = self.cache.write().unwrap();
        *cache = Some(CacheEntry {
            value: Some(value.clone()),
            dirty: true,
        });
    }

    pub fn flush(&self) {
        let mut cache = self.cache.write().unwrap();
        if let Some(cache) = cache.as_mut() {
            if cache.dirty {
                if let Some(value) = cache.value.as_ref() {
                    log::info!("Flushing KVS: {}", self.name);
                    self.set_direct(value);
                }
                cache.dirty = false;
            }
        }
    }
}
