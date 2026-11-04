//! Small LRU cache, the same policy as `LRUCache` in the Python service.

use std::collections::{HashMap, VecDeque};

#[derive(Debug)]
pub struct LruCache {
    capacity: usize,
    entries: HashMap<String, Vec<u8>>,
    order: VecDeque<String>,
}

impl LruCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::with_capacity(capacity.max(1)),
            order: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        let value = self.entries.get(key)?.clone();
        self.touch(key);
        Some(value)
    }

    pub fn put(&mut self, key: String, value: Vec<u8>) {
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        } else {
            self.touch(&key);
        }

        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
        }
        self.order.push_back(key.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_cached_values() {
        let mut cache = LruCache::new(2);
        cache.put("a".into(), vec![1]);
        assert_eq!(cache.get("a"), Some(vec![1]));
        assert_eq!(cache.get("missing"), None);
    }

    #[test]
    fn evicts_the_least_recently_used() {
        let mut cache = LruCache::new(2);
        cache.put("a".into(), vec![1]);
        cache.put("b".into(), vec![2]);
        assert_eq!(cache.get("a"), Some(vec![1]));
        cache.put("c".into(), vec![3]);
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("a"), Some(vec![1]));
        assert_eq!(cache.get("c"), Some(vec![3]));
    }
}
