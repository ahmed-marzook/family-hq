//! A small in-memory cache whose entries expire, for API answers that are
//! fine to reuse for a while (the weather for 15 minutes, say).
//!
//! It lives in the process, so a restart empties it. That's fine: the
//! first request after a restart just asks the API again.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono_tz::Tz;

use crate::geo::Coordinates;

/// Remembers values for `ttl` ("time to live") after they're stored.
pub struct TtlCache<K, V> {
    ttl: Duration,
    /// A std `Mutex`: it's never held across an `.await`.
    entries: Mutex<HashMap<K, (Instant, V)>>,
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + Hash,
    V: Clone,
{
    pub fn new(ttl: Duration) -> Self {
        TtlCache {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// The value stored for `key`, if it's younger than `ttl` at `now`.
    ///
    /// `now` is passed in (rather than read here) so tests can move time
    /// forward without waiting.
    pub fn get(&self, key: &K, now: Instant) -> Option<V> {
        // `lock()` only fails if another thread panicked while holding the
        // lock. The map is still usable then, so take it anyway with
        // `into_inner` rather than crash this request too.
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let (stored, value) = entries.get(key)?;
        // `saturating_duration_since` is 0 rather than a panic if `now`
        // were somehow before `stored`.
        (now.saturating_duration_since(*stored) < self.ttl).then(|| value.clone())
    }

    /// Stores `value` for `key` as of `now`, and drops anything expired so
    /// the cache never grows past what's been used within `ttl`.
    pub fn insert(&self, key: K, value: V, now: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, (stored, _)| now.saturating_duration_since(*stored) < self.ttl);
        entries.insert(key, (now, value));
    }

    /// How many entries there are, expired or not (for tests).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

/// A place as a cache key: coordinates to 4 decimal places (about 10 m, as
/// `Location` stores them) and the timezone the answer was asked in.
///
/// `f64` can't be a `HashMap` key (it has no `Eq`: `NaN != NaN`), so the
/// coordinates are turned into whole numbers first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaceKey {
    latitude_e4: i32,
    longitude_e4: i32,
    timezone: Tz,
}

impl PlaceKey {
    pub fn new(at: Coordinates, timezone: Tz) -> Self {
        PlaceKey {
            latitude_e4: (at.latitude * 10_000.0).round() as i32,
            longitude_e4: (at.longitude * 10_000.0).round() as i32,
            timezone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn a_value_is_kept_until_it_expires() {
        let cache = TtlCache::new(15 * MINUTE);
        let start = Instant::now();
        cache.insert("mk", 9.3, start);

        assert_eq!(cache.get(&"mk", start), Some(9.3));
        assert_eq!(cache.get(&"mk", start + 14 * MINUTE), Some(9.3));
        assert_eq!(cache.get(&"mk", start + 15 * MINUTE), None, "expired");
        assert_eq!(cache.get(&"leeds", start), None, "never stored");
    }

    #[test]
    fn storing_again_starts_the_clock_again() {
        let cache = TtlCache::new(15 * MINUTE);
        let start = Instant::now();
        cache.insert("mk", 9.3, start);
        cache.insert("mk", 10.1, start + 10 * MINUTE);
        assert_eq!(cache.get(&"mk", start + 20 * MINUTE), Some(10.1));
    }

    #[test]
    fn expired_entries_are_dropped_when_something_is_added() {
        let cache = TtlCache::new(15 * MINUTE);
        let start = Instant::now();
        cache.insert("mk", 1, start);
        cache.insert("leeds", 2, start + 5 * MINUTE);
        cache.insert("york", 3, start + 16 * MINUTE);
        // "mk" (16 min old) is gone; "leeds" (11 min) and "york" stay.
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&"leeds", start + 16 * MINUTE), Some(2));
    }

    #[test]
    fn nearby_points_share_a_place_key() {
        let london = chrono_tz::Europe::London;
        let at = |latitude, longitude| Coordinates {
            latitude,
            longitude,
        };
        let mk = PlaceKey::new(at(52.04171, -0.75581), london);
        assert_eq!(mk, PlaceKey::new(at(52.041_74, -0.755_76), london));
        assert_ne!(
            mk,
            PlaceKey::new(at(52.0427, -0.7558), london),
            "~100 m away"
        );
        assert_ne!(
            mk,
            PlaceKey::new(at(52.04171, -0.75581), chrono_tz::Asia::Dubai),
            "same place, asked in another timezone"
        );
    }
}
