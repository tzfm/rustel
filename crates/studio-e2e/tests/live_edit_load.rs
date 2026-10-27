//! Edits of a playing score land while the machine is busy. The replacement
//! takeover must keep the headroom the producer's own measured turns need,
//! so a save of an impure score - one whose every scheduling window queries
//! JavaScript - arrives late rather than rolling back with a takeover
//! refusal, even with busy cores competing for the CPU.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustel_studio_e2e::hermetic;

/// Busy cores that stop when the test ends, however it ends.
struct Spinners {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Spinners {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn spinners(count: usize) -> Spinners {
    let stop = Arc::new(AtomicBool::new(false));
    let threads = (0..count)
        .map(|index| {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("e2e-busy-{index}"))
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
                .expect("busy thread")
        })
        .collect();
    Spinners { stop, threads }
}

#[test]
fn impure_edits_land_while_busy_cores_spin() {
    let mut studio = hermetic();

    studio.chord("ctrl+s");
    studio.pump_until(
        Duration::from_secs(30),
        "the starter score is playing",
        |studio| studio.is_playing(),
    );

    // Three cores of ordinary busy work, the way a browser beside the studio
    // loads a small machine. The output is the harness's silent one; what
    // this stresses is the replacement's own timing under contention.
    let _busy = spinners(3);

    let saves = [
        (
            "impure supersaw lane",
            "$: s(\"bd\") // one\n$: n(rand.range(0,12)).scale(\"c:major\").s(\"supersaw*16\")\n",
        ),
        (
            "impure lane with a filter sweep",
            "$: s(\"bd\") // two\n$: n(irand(16).seg(8)).scale(\"d:phrygian\").s(\"supersaw\").djf(\"<.5 .3 .2 .75>\")\n",
        ),
        ("back to a pure score", "$: s(\"bd\") // three\n"),
    ];
    for (step, (what, score)) in saves.iter().enumerate() {
        let before = studio.log_texts().len();
        studio.set_score(score);
        studio.chord("ctrl+s");
        // The settle of an update under load is slow but bounded: the
        // evaluation answers, the producer arms, the window publishes.
        studio.pump_until(
            Duration::from_secs(120),
            "the engine answered the update",
            |studio| !studio.is_evaluating() && studio.is_playing(),
        );
        studio.settle();

        let refusals = studio
            .log_texts()
            .iter()
            .skip(before)
            .filter(|line| line.contains("replacement prefill finished after its takeover"))
            .count();
        let allowed = usize::from(step == 0);
        assert!(
            refusals <= allowed,
            "step {step} ({what}) rolled {refusals} replacements back, {allowed} allowed"
        );
    }
}
