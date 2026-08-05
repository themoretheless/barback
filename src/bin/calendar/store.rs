use std::fs;
use std::io;
use std::path::PathBuf;

use crate::event::Event;

pub struct Store {
    path: PathBuf,
    pub events: Vec<Event>,
}

impl Store {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let events = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, events })
    }

    pub fn save(&self) -> io::Result<()> {
        let text = serde_json::to_string_pretty(&self.events)?;
        fs::write(&self.path, text)
    }

    pub fn next_id(&self) -> u64 {
        self.events.iter().map(|e| e.id).max().unwrap_or(0) + 1
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.events.len();
        self.events.retain(|e| e.id != id);
        self.events.len() != before
    }
}
