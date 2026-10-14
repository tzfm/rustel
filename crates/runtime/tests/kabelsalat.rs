//! Score-to-audio checks for graph sources, effects, and replacement errors.

use rustel_fraction::Fraction;
use rustel_runtime::{QUERY_WORKER_STACK_BYTES, Session};
use std::sync::atomic::AtomicBool;

fn on_score_thread(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(QUERY_WORKER_STACK_BYTES)
        .spawn(test)
        .expect("score thread")
        .join()
        .expect("score test");
}

fn render(source: &str) -> Vec<f32> {
    let mut session = Session::new().expect("session");
    session.set_direct_diagnostic_logging(false);
    session.evaluate(source).expect("score");
    let pcm = session.render_pcm(0.1).expect("render");
    assert!(pcm.iter().all(|sample| sample.is_finite()));
    assert!(pcm.iter().any(|sample| sample.abs() > 0.001));
    pcm
}

#[test]
fn graph_sources_and_effects_reach_the_offline_renderer() {
    on_score_thread(|| {
        let direct = render("K(n(0.2).out())");
        let effect = render("K(audioin().mul(2).out()).FX(K(n(0.1).out()).gain(1))");
        assert_eq!(direct.len(), effect.len());
        assert!(
            direct
                .iter()
                .zip(&effect)
                .all(|(left, right)| (left - right).abs() < 1e-6)
        );
        let stereo = render("K(n([0.05,0.1]).out([0,1]))");
        assert!(
            stereo
                .as_chunks::<2>()
                .0
                .iter()
                .all(|frame| (frame[1] - 2.0 * frame[0]).abs() < 1e-6)
        );
    });
}

#[test]
fn invalid_graph_replacements_keep_the_playing_score() {
    on_score_thread(|| {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let stopped = AtomicBool::new(false);
        session
            .reload_at_cancellable("pure('playing')", false, 0.0, &stopped)
            .expect("initial score");
        for source in [
            "K(midifreq().out())",
            "K(n(1).raw('unknown(t)').out())",
            "K(n(Infinity).out())",
        ] {
            let error = match session.reload_at_cancellable(source, false, 0.1, &stopped) {
                Err(error) => error,
                Ok(generation) => panic!("accepted invalid graph {source}: {generation}"),
            };
            assert!(!error.to_string().is_empty());
            let haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("old score query");
            assert_eq!(haps.len(), 1, "{source}");
            assert_eq!(
                haps[0].value,
                rustel_core::Value::Str("playing".into()),
                "{source}"
            );
        }
    });
}
