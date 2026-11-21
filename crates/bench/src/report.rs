//! A run's measurements: a table to read, JSON to keep, and the rules
//! that call a change a regression.

use crate::measure::Cost;
use crate::seams::Seam;
use serde_json::{Value, json};

/// One score's seams by name, or the reason it has none.
type Measured = Result<Vec<(String, Cost)>, String>;

/// Every score of a run, with its seams or the reason it has none.
#[derive(Debug, Default)]
pub struct Report {
    scores: Vec<(String, Measured)>,
}

impl Report {
    pub fn push(&mut self, score: String, seams: Result<Vec<Seam>, String>) {
        let seams = seams.map(|seams| {
            seams
                .into_iter()
                .map(|(name, cost)| (name.to_owned(), cost))
                .collect()
        });
        self.scores.push((score, seams));
    }

    /// One line per seam: times in microseconds, memory in kilobytes.
    pub fn table(&self) -> String {
        let mut out = format!(
            "{:<44} {:<20} {:>10} {:>10} {:>9} {:>10} {:>9}\n",
            "score", "seam", "wall us", "cpu us", "allocs", "held KB", "peak KB"
        );
        for (score, seams) in &self.scores {
            let name = short(score);
            match seams {
                Err(error) => out.push_str(&format!("{name:<44} error: {error}\n")),
                Ok(seams) => {
                    for (seam, cost) in seams {
                        out.push_str(&format!(
                            "{name:<44} {seam:<20} {:>10.1} {:>10.1} {:>9} {:>10.1} {:>9.1}\n",
                            cost.wall_ns as f64 / 1e3,
                            cost.cpu_ns as f64 / 1e3,
                            cost.allocations,
                            cost.held as f64 / 1024.0,
                            cost.peak as f64 / 1024.0,
                        ));
                    }
                }
            }
        }
        out
    }

    pub fn to_json(&self) -> String {
        let scores: Vec<Value> = self
            .scores
            .iter()
            .map(|(score, seams)| match seams {
                Err(error) => json!({ "score": score, "error": error }),
                Ok(seams) => json!({
                    "score": score,
                    "seams": seams.iter().map(|(seam, cost)| json!({
                        "seam": seam,
                        "wall_ns": cost.wall_ns,
                        "cpu_ns": cost.cpu_ns,
                        "allocations": cost.allocations,
                        "allocated": cost.allocated,
                        "held": cost.held,
                        "peak": cost.peak,
                    })).collect::<Vec<_>>(),
                }),
            })
            .collect();
        serde_json::to_string_pretty(&json!({ "scores": scores })).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        let scores = root["scores"].as_array().ok_or("no scores")?;
        let number = |seam: &Value, field: &str| seam[field].as_u64().unwrap_or(0);
        let mut report = Self::default();
        for entry in scores {
            let score = entry["score"]
                .as_str()
                .ok_or("a score without a name")?
                .to_owned();
            let seams = match entry["error"].as_str() {
                Some(error) => Err(error.to_owned()),
                None => Ok(entry["seams"]
                    .as_array()
                    .ok_or("a score without seams")?
                    .iter()
                    .map(|seam| {
                        let cost = Cost {
                            wall_ns: number(seam, "wall_ns"),
                            cpu_ns: number(seam, "cpu_ns"),
                            allocations: number(seam, "allocations"),
                            allocated: number(seam, "allocated"),
                            held: seam["held"].as_i64().unwrap_or(0),
                            peak: number(seam, "peak"),
                        };
                        (seam["seam"].as_str().unwrap_or_default().to_owned(), cost)
                    })
                    .collect()),
            };
            report.scores.push((score, seams));
        }
        Ok(report)
    }
}

/// How far a measure may grow before it counts as a regression: a share of
/// the base, plus a floor that keeps tiny values from flagging on noise.
struct Tolerance {
    share: f64,
    floor: f64,
}

/// Counts and bytes do not depend on machine load, so they gate tightly.
const ALLOCATIONS: Tolerance = Tolerance {
    share: 0.02,
    floor: 16.0,
};
const HELD: Tolerance = Tolerance {
    share: 0.05,
    floor: 4096.0,
};
const PEAK: Tolerance = Tolerance {
    share: 0.10,
    floor: 65536.0,
};
/// Time moves with load, so it gates loosely: compare runs made on one
/// machine in one session.
const TIME: Tolerance = Tolerance {
    share: 0.25,
    floor: 2000.0,
};

impl Tolerance {
    fn exceeded(&self, base: f64, new: f64) -> bool {
        new > base * (1.0 + self.share) + self.floor
    }
}

/// Which measures a comparison gates on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    All,
    /// Counts and bytes only, for runs made on shared machines whose time
    /// moves with their neighbours' load.
    MemoryOnly,
}

/// Every seam that got worse from `base` to `new` past its tolerance, and
/// every score that evaluated in `base` and fails in `new`.
pub fn regressions(base: &Report, new: &Report, gate: Gate) -> Vec<String> {
    let mut out = Vec::new();
    for (score, base_seams) in &base.scores {
        let Ok(base_seams) = base_seams else {
            continue;
        };
        let Some((_, new_seams)) = new.scores.iter().find(|(name, _)| name == score) else {
            continue;
        };
        let new_seams = match new_seams {
            Ok(seams) => seams,
            Err(error) => {
                out.push(format!("{}: now fails: {error}", short(score)));
                continue;
            }
        };
        for (seam, base_cost) in base_seams {
            let Some((_, new_cost)) = new_seams.iter().find(|(name, _)| name == seam) else {
                continue;
            };
            let checks: [(&str, &Tolerance, f64, f64); 5] = [
                (
                    "cpu",
                    &TIME,
                    base_cost.cpu_ns as f64,
                    new_cost.cpu_ns as f64,
                ),
                (
                    "wall",
                    &TIME,
                    base_cost.wall_ns as f64,
                    new_cost.wall_ns as f64,
                ),
                (
                    "allocations",
                    &ALLOCATIONS,
                    base_cost.allocations as f64,
                    new_cost.allocations as f64,
                ),
                ("held", &HELD, base_cost.held as f64, new_cost.held as f64),
                ("peak", &PEAK, base_cost.peak as f64, new_cost.peak as f64),
            ];
            for (measure, tolerance, before, after) in checks {
                let timed = matches!(measure, "cpu" | "wall");
                if (gate == Gate::All || !timed) && tolerance.exceeded(before, after) {
                    out.push(format!(
                        "{} {seam} {measure}: {before:.0} -> {after:.0}",
                        short(score)
                    ));
                }
            }
        }
    }
    out
}

/// A score's file name, enough to tell scores apart in a table.
fn short(score: &str) -> &str {
    score.rsplit(['/', '\\']).next().unwrap_or(score)
}

#[cfg(test)]
mod tests {
    use super::{Gate, Report, regressions};
    use crate::measure::Cost;

    fn report(allocations: u64, held: i64) -> Report {
        let mut report = Report::default();
        let cost = Cost {
            allocations,
            held,
            ..Cost::default()
        };
        report.push("songs/a.strudel".into(), Ok(vec![("steady query", cost)]));
        report.push(
            "songs/b.strudel".into(),
            Ok(vec![("steady query", Cost::default())]),
        );
        report
    }

    #[test]
    fn growth_past_a_tolerance_is_a_regression_and_a_round_trip_keeps_it() {
        let base = Report::from_json(&report(1000, 1 << 20).to_json()).expect("a report");
        assert!(regressions(&base, &report(1010, 1 << 20), Gate::All).is_empty());
        let worse = regressions(&base, &report(1100, 2 << 20), Gate::MemoryOnly);
        assert_eq!(worse.len(), 2, "{worse:?}");
        let mut failed = report(1000, 1 << 20);
        failed.scores[1].1 = Err("refused".into());
        assert_eq!(
            regressions(&base, &failed, Gate::All),
            ["b.strudel: now fails: refused"]
        );
    }
}
