//! `rustel bench`: measure evaluation, steady query throughput, CPU time,
//! allocations, and live/peak heap memory for a score or `-e` expression.

use std::time::{Duration, Instant};

use rustel_audio::tripwire::{AllocMetrics, measure_allocations};
use rustel_runtime::{BenchMetrics, RuntimeError, Session, outln};
use serde::Serialize;

use crate::{
    EVALUATION_CANCELLED, SourceInput, evaluate_source, json_err, parse_fraction, read_source,
    session_config, style, watch_for_interrupt, with_sample_access,
};

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct SeamReport {
    wall_us: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_us: Option<f64>,
    allocations: u64,
    allocated_bytes: u64,
    held_bytes: i64,
    peak_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
struct BenchReport {
    #[serde(flatten)]
    query: BenchMetrics,
    engine: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_secs: Option<f64>,
    eval: SeamReport,
    steady_query: SeamReport,
    session_held_bytes: u64,
    tables_held_bytes: u64,
}

struct Timed<T> {
    value: T,
    wall: Duration,
    cpu: Option<Duration>,
    alloc: AllocMetrics,
}

fn measure_seam<T>(work: impl FnOnce() -> T) -> Timed<T> {
    let cpu_before = cpu_time();
    let started = Instant::now();
    let (value, alloc) = measure_allocations(work);
    let wall = started.elapsed();
    let cpu = match (cpu_before, cpu_time()) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    Timed {
        value,
        wall,
        cpu,
        alloc,
    }
}

#[cfg(unix)]
fn cpu_time() -> Option<Duration> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage(RUSAGE_SELF, ...)` writes the current process's
    // resource usage into the provided `rusage` struct.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `getrusage` returned 0, so `usage` is fully initialized.
    let usage = unsafe { usage.assume_init() };
    let tv = |value: libc::timeval| {
        Duration::new(
            u64::try_from(value.tv_sec).unwrap_or(0),
            u32::try_from(value.tv_usec)
                .unwrap_or(0)
                .saturating_mul(1_000),
        )
    };
    Some(tv(usage.ru_utime) + tv(usage.ru_stime))
}

#[cfg(not(unix))]
fn cpu_time() -> Option<Duration> {
    None
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_signed_bytes(bytes: i64) -> String {
    if bytes < 0 {
        format!("-{}", format_bytes(bytes.unsigned_abs()))
    } else {
        format_bytes(bytes as u64)
    }
}

fn format_us(us: f64) -> String {
    if us < 1_000.0 {
        format!("{us:.1} us")
    } else if us < 1_000_000.0 {
        format!("{:.2} ms", us / 1_000.0)
    } else {
        format!("{:.3} s", us / 1_000_000.0)
    }
}

pub(crate) fn run_bench(
    dispatch: rustel_audio::DspDispatch,
    input: &SourceInput,
    begin: &str,
    end: &str,
    iterations: u64,
    json: bool,
) -> Result<(), RuntimeError> {
    if iterations == 0 {
        return Err(RuntimeError::Message(
            "bench iterations must be greater than 0".into(),
        ));
    }
    let source = read_source(input)?;
    let begin = parse_fraction(begin)?;
    let end = parse_fraction(end)?;
    let config = with_sample_access(session_config(dispatch), &input.sample_access)?;

    // Warm process-wide static registries with one native throwaway session so
    // the measured eval reflects the score itself rather than one-time tables.
    if let Ok(mut warm) = Session::with_config(config.clone()) {
        let _ = warm.evaluate("pure(1)");
        drop(warm);
    }
    rustel_runtime::forget_mini_literals();

    let mut session = Session::with_config(config)?;
    let _watcher = watch_for_interrupt(session.transport());

    let eval_timed = measure_seam(|| evaluate_source(&mut session, &source, &EVALUATION_CANCELLED));
    eval_timed.value?;

    let engine = if session.evaluated_natively() {
        "native"
    } else {
        "script"
    };

    // Warm one query outside the timed loop so one-time query caches stay out
    // of steady-state measurements, matching `Session::bench`.
    let _ = session.query(begin, end)?;

    let query_timed = measure_seam(|| -> Result<u64, RuntimeError> {
        let mut total_haps = 0u64;
        for _ in 0..iterations {
            if session.transport().is_stopped() {
                return Err(RuntimeError::Cancelled);
            }
            total_haps += session.query(begin, end)?.len() as u64;
        }
        Ok(total_haps)
    });
    let total_haps = query_timed.value?;

    let tables_held_bytes = rustel_core::table_bytes() as u64;
    let (_, dropped) = measure_allocations(|| {
        drop(session);
        rustel_runtime::forget_mini_literals();
    });
    let session_held_bytes = (-dropped.held_bytes).max(0) as u64;

    let elapsed_secs = query_timed.wall.as_secs_f64().max(f64::EPSILON);
    let cpu_secs = query_timed.cpu.map(|d| d.as_secs_f64());
    let iters_f = iterations as f64;

    let eval = SeamReport {
        wall_us: eval_timed.wall.as_secs_f64() * 1e6,
        cpu_us: eval_timed.cpu.map(|d| d.as_secs_f64() * 1e6),
        allocations: eval_timed.alloc.allocations,
        allocated_bytes: eval_timed.alloc.allocated_bytes,
        held_bytes: eval_timed.alloc.held_bytes,
        peak_bytes: eval_timed.alloc.peak_bytes,
    };
    let steady_query = SeamReport {
        wall_us: query_timed.wall.as_secs_f64() * 1e6 / iters_f,
        cpu_us: query_timed.cpu.map(|d| d.as_secs_f64() * 1e6 / iters_f),
        allocations: query_timed.alloc.allocations / iterations,
        allocated_bytes: query_timed.alloc.allocated_bytes / iterations,
        held_bytes: query_timed.alloc.held_bytes,
        peak_bytes: query_timed.alloc.peak_bytes,
    };

    let report = BenchReport {
        query: BenchMetrics {
            source,
            begin: begin.show(),
            end: end.show(),
            iterations,
            total_haps,
            haps_per_iteration: total_haps as f64 / iters_f,
            elapsed_secs,
            queries_per_sec: iters_f / elapsed_secs,
            haps_per_sec: total_haps as f64 / elapsed_secs,
        },
        engine,
        cpu_secs,
        eval,
        steady_query,
        session_held_bytes,
        tables_held_bytes,
    };

    if json {
        outln!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(json_err)?
        );
    } else {
        let on = style::stdout_on();
        let eval_cpu = report
            .eval
            .cpu_us
            .map(|us| format!(", {} cpu", format_us(us)))
            .unwrap_or_default();
        let query_cpu = report
            .steady_query
            .cpu_us
            .map(|us| format!(", {} cpu", format_us(us)))
            .unwrap_or_default();
        outln!("engine   {}", style::bold(on, report.engine));
        outln!(
            "eval     {} wall{}, {} allocs, {} held, {} peak",
            style::bold(on, &format_us(report.eval.wall_us)),
            eval_cpu,
            report.eval.allocations,
            format_signed_bytes(report.eval.held_bytes),
            format_bytes(report.eval.peak_bytes),
        );
        outln!(
            "query    {} wall{}, {} allocs/query, {} peak",
            style::bold(on, &format_us(report.steady_query.wall_us)),
            query_cpu,
            report.steady_query.allocations,
            format_bytes(report.steady_query.peak_bytes),
        );
        outln!(
            "         {} queries in {:.3} s - {} queries/s, {} haps/s, {} haps a query",
            style::bold(on, &report.query.iterations.to_string()),
            report.query.elapsed_secs,
            style::bold(on, &format!("{:.0}", report.query.queries_per_sec)),
            style::bold(on, &format!("{:.0}", report.query.haps_per_sec)),
            report.query.haps_per_iteration,
        );
        outln!(
            "memory   {} session held, {} tables",
            style::bold(on, &format_bytes(report.session_held_bytes)),
            format_bytes(report.tables_held_bytes),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_scales_bytes_and_microseconds() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.00 MB");
        assert_eq!(format_signed_bytes(-2048), "-2.0 KB");
        assert_eq!(format_signed_bytes(0), "0 B");
        assert_eq!(format_us(12.34), "12.3 us");
        assert_eq!(format_us(2_500.0), "2.50 ms");
        assert_eq!(format_us(1_500_000.0), "1.500 s");
    }

    #[test]
    fn tripwire_alloc_measures_allocations_peak_and_release() {
        let (buf, alloc) = measure_allocations(|| {
            let mut v = Vec::<u8>::with_capacity(4096);
            v.resize(4096, 1);
            std::hint::black_box(v)
        });
        assert!(alloc.allocations >= 1);
        assert!(alloc.allocated_bytes >= 4096);
        assert!(alloc.held_bytes >= 4096);
        assert!(alloc.peak_bytes >= 4096);

        let (_, freed) = measure_allocations(|| {
            drop(buf);
        });
        assert!(freed.held_bytes <= -4096);
    }
}
