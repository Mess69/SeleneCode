use std::collections::HashMap;
use crate::error::Error;

pub struct Registry {
    entries: HashMap<String, String>,
}

pub trait Lookup {
    fn lookup(&self, key: &str) -> Option<String>;
}

impl Lookup for Registry {
    fn lookup(&self, key: &str) -> Option<String> {
        self.entries.get(key).cloned()
    }
}

impl Registry {
    pub fn new() -> Registry {
        Registry { entries: HashMap::new() }
    }

    pub fn insert(&mut self, key: &str, value: &str) {
        self.entries.insert(key.to_string(), value.to_string());
    }
}

pub fn build() -> Registry {
    let mut r = Registry::new();
    r.insert("a", "b");
    r
}
