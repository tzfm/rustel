//! Native Csound instruments with compilation and synthesis on a worker.

mod engine;
mod worker;

use rustel_audio::{InsertKey, OrbitInsert};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub const PLUGIN_ID: u32 = u32::MAX;
const MAX_EVENTS: usize = 65_536;
const MAX_EVENT_BYTES: usize = 64 * 1024 * 1024;
static NEXT_PROGRAM: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Debug)]
pub enum Instrument {
    Named(String),
    Number(f64),
}

#[derive(Clone, Debug)]
pub struct Event {
    pub instrument: Instrument,
    pub p4: f64,
    pub p5: f64,
    pub p6: String,
    pub duration: f64,
}

impl Event {
    /// Checks score fields without compiling an orchestra or retaining a note.
    pub fn validate(&self) -> Result<(), String> {
        self.score().map(|_| ())
    }

    fn score(&self) -> Result<String, String> {
        if !self.p4.is_finite()
            || !self.p5.is_finite()
            || !self.duration.is_finite()
            || self.duration < 0.0
        {
            return Err("Csound needs finite p-fields and a nonnegative duration".into());
        }
        let instrument = match &self.instrument {
            Instrument::Named(name) if !name.is_empty() => quote(name)?,
            Instrument::Number(n) if n.is_finite() && *n != 0.0 => n.to_string(),
            _ => return Err("Csound needs an instrument name or nonzero number".into()),
        };
        Ok(format!(
            "i {instrument} 0 {} {} {} {}\n",
            self.duration,
            self.p4,
            self.p5,
            quote(&self.p6)?
        ))
    }
}

fn quote(value: &str) -> Result<String, String> {
    if value.chars().any(|c| c.is_control()) {
        return Err("Csound score strings cannot contain control characters".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[derive(Default)]
struct Events {
    next: u64,
    bytes: usize,
    scores: BTreeMap<u64, String>,
}

/// A validated orchestra revision and its prepared score events.
#[derive(Clone)]
pub struct Program {
    key: InsertKey,
    sources: Arc<Vec<String>>,
    events: Arc<Mutex<Events>>,
    errors: Arc<Mutex<Vec<String>>>,
    instances: Arc<AtomicUsize>,
}

impl Program {
    /// Compiles all sources before the host replaces the playing score.
    pub fn compile(sources: &[String]) -> Result<Self, String> {
        let sources = Arc::new(sources.to_vec());
        engine::Engine::new(&sources, 48_000)?;
        let preset = NEXT_PROGRAM
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| "Csound orchestra revision limit reached")?;
        Ok(Self {
            key: InsertKey {
                plugin: PLUGIN_ID,
                preset,
            },
            sources,
            events: Arc::new(Mutex::new(Events::default())),
            errors: Arc::new(Mutex::new(Vec::new())),
            instances: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn key(&self) -> InsertKey {
        self.key
    }

    /// Retains bounded worker diagnostics for the host's next update.
    pub fn report_error(&self, mut error: String) {
        let mut end = error.len().min(8192);
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        if let Ok(mut errors) = self.errors.lock()
            && errors.len() < 16
        {
            errors.push(error);
        }
    }

    pub fn take_errors(&self) -> Vec<String> {
        self.errors
            .lock()
            .map_or_else(|_| Vec::new(), |mut errors| std::mem::take(&mut *errors))
    }

    /// Stores a score string outside the audio callback. Each ID plays once.
    pub fn prepare(&self, event: Event) -> Result<u64, String> {
        let score = event.score()?;
        let mut events = self
            .events
            .lock()
            .map_err(|_| "Csound event store is unavailable")?;
        if events.scores.len() >= MAX_EVENTS
            || score.len() > MAX_EVENT_BYTES.saturating_sub(events.bytes)
        {
            return Err("Csound prepared event limit reached".into());
        }
        events.next = events
            .next
            .checked_add(1)
            .ok_or("Csound event ID limit reached")?;
        let id = events.next;
        events.bytes += score.len();
        events.scores.insert(id, score);
        Ok(id)
    }

    pub fn pending_events(&self) -> usize {
        self.events.lock().map_or(0, |events| events.scores.len())
    }

    /// Counts synthesis workers until their native engines have shut down.
    pub fn active_instances(&self) -> usize {
        self.instances.load(Ordering::Acquire)
    }

    pub fn discard(&self, id: u64) {
        let _ = self.take(id);
    }

    /// Drops unsubmitted events after a preview or a transport reset.
    pub fn clear_events(&self) {
        if let Ok(mut events) = self.events.lock() {
            events.scores.clear();
            events.bytes = 0;
        }
    }

    fn take(&self, id: u64) -> Option<String> {
        let mut events = self.events.lock().ok()?;
        let score = events.scores.remove(&id)?;
        events.bytes -= score.len();
        Some(score)
    }

    pub fn instance(&self, sample_rate: u32) -> Result<Box<dyn OrbitInsert>, String> {
        Ok(Box::new(worker::Insert::new(self.clone(), sample_rate)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_strings_cannot_inject_more_events() {
        let mut event = Event {
            instrument: Instrument::Named("glass".into()),
            p4: 440.0,
            p5: 0.2,
            p6: "cutoff/900\"/s/glass".into(),
            duration: 0.5,
        };
        assert_eq!(
            event.score().unwrap(),
            "i \"glass\" 0 0.5 440 0.2 \"cutoff/900\\\"/s/glass\"\n"
        );
        event.p6 = "x\ni 1 0 10".into();
        assert!(event.score().is_err());
        event.p6.clear();
        event.duration = f64::NAN;
        assert!(event.score().is_err());
    }
}
