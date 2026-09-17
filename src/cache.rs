use std::collections::HashMap;
use std::time::{Duration, Instant};

struct Entry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
}

#[derive(Default)]
pub struct Cache {
    entries: HashMap<String, Entry>,
}

impl Cache {
    pub fn get(&mut self, key: &str, now: Instant) -> Option<&[u8]> {
        if self
            .entries
            .get(key)
            .is_some_and(|e| e.expires_at.is_some_and(|t| t <= now))
        {
            self.entries.remove(key);
        }
        self.entries.get(key).map(|e| e.value.as_slice())
    }

    pub fn put(&mut self, key: String, value: Vec<u8>, ttl: Option<Duration>, now: Instant) {
        let expires_at = ttl.map(|t| now + t);
        self.entries.insert(key, Entry { value, expires_at });
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Live keys with how long each has left, sorted by key.
    pub fn entries(&self, now: Instant) -> Vec<(String, Option<Duration>)> {
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| e.expires_at.is_none_or(|t| t > now))
            .map(|(k, e)| (k.clone(), e.expires_at.map(|t| t - now)))
            .collect();
        entries.sort();
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_expire_only_when_given_a_ttl() {
        let mut cache = Cache::default();
        let t0 = Instant::now();
        cache.put("forever".into(), b"a".to_vec(), None, t0);
        cache.put(
            "brief".into(),
            b"b".to_vec(),
            Some(Duration::from_secs(10)),
            t0,
        );

        let later = t0 + Duration::from_secs(5);
        assert_eq!(cache.get("forever", later), Some(&b"a"[..]));
        assert_eq!(cache.get("brief", later), Some(&b"b"[..]));
        assert_eq!(
            cache.entries(later),
            vec![
                ("brief".into(), Some(Duration::from_secs(5))),
                ("forever".into(), None)
            ]
        );

        let expired = t0 + Duration::from_secs(10);
        assert_eq!(cache.entries(expired), vec![("forever".into(), None)]);
        assert_eq!(cache.get("brief", expired), None);
        assert_eq!(cache.get("forever", expired), Some(&b"a"[..]));

        cache.clear();
        assert!(cache.entries(expired).is_empty());
    }
}
