use crate::assertions::{self, compiled};
use associative_cache::{Capacity8192, HashDirectMapped, LruReplacement, LruTimestamp};
use std::{
    fmt,
    sync::{Arc, RwLock},
    time::Instant,
};

pub struct CacheEntry {
    assertion: Arc<compiled::Assertion>,
    timestamp: RwLock<Instant>,
}

impl LruTimestamp for CacheEntry {
    type Timestamp<'a> = Instant;

    fn get_timestamp(&self) -> Self::Timestamp<'_> {
        *self.timestamp.read().expect("not poisoned")
    }

    fn update_timestamp(&self) {
        *self.timestamp.write().expect("not poisoned") = Instant::now();
    }
}

pub struct Cache {
    compiled: Arc<RwLock<AssocCache>>,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache").finish()
    }
}

type AssocCache = associative_cache::AssociativeCache<
    assertions::Assertion,
    CacheEntry,
    Capacity8192,
    HashDirectMapped,
    LruReplacement,
>;

impl Cache {
    pub fn new() -> Self {
        Self {
            compiled: Arc::new(RwLock::new(AssocCache::default())),
        }
    }
    pub fn get_or_compile(
        &self,
        key: &assertions::Assertion,
    ) -> Result<Arc<compiled::Assertion>, compiled::Error> {
        {
            let rl = self.compiled.read().expect("not poisoned");

            if let Some(entry) = rl.get(key) {
                return Ok(entry.assertion.clone());
            }
        }
        let comp = compiled::compile(key);
        match comp {
            Ok(comp) => {
                let mut wl = self.compiled.write().expect("not poisoned");
                let comp = Arc::new(comp);
                wl.insert(
                    key.clone(),
                    CacheEntry {
                        assertion: comp.clone(),
                        timestamp: RwLock::new(Instant::now()),
                    },
                );

                Ok(comp)
            }
            Err(err) => Err(err),
        }
    }
}
