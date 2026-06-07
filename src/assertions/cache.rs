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
        max_assertion_ops: u32,
        region: &'static str,
    ) -> Result<Arc<compiled::Assertion>, compiled::CompilationError> {
        let start = Instant::now();

        if let Some(entry) = self.compiled.read().expect("not poisoned").get(key) {
            let result = Ok(entry.assertion.clone());

            metrics::histogram!("assertion.cache_hit", "histogram" => "timer", "uptime_region" => region)
                .record(start.elapsed().as_secs_f64());
            return result;
        }

        let comp = compiled::compile(key, max_assertion_ops, region);
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

                metrics::histogram!("assertion.cache_miss", "histogram" => "timer", "uptime_region" => region)
                    .record(start.elapsed().as_secs_f64());

                Ok(comp)
            }
            Err(err) => Err(err),
        }
    }
}
