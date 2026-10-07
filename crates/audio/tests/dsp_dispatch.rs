use rustel_audio::reverb::{IrParams, OrbitReverb, ReverbParams};
use rustel_audio::{
    AudioBackend, ConvolutionKernelKind, DecodedSample, DspDispatch, FxStage, LiveScalarBackend,
    OnsetEvent, ReverbControls, SampleId, ScalarBackend, SupersawKernelKind, SynthSource,
    WavetableKernelKind, render_pcm,
};

const SAMPLE_RATE: u32 = 24_000;
const IR_SAMPLE: SampleId = SampleId(42);

fn assert_portable(dispatch: DspDispatch) {
    assert!(dispatch.is_forced_portable());
    assert_eq!(
        dispatch.convolution_kernel_kind(),
        ConvolutionKernelKind::Portable
    );
    assert_eq!(
        dispatch.supersaw_kernel_kind(),
        SupersawKernelKind::Portable
    );
    assert_eq!(
        dispatch.wavetable_kernel_kind(),
        WavetableKernelKind::Portable
    );
}

fn assert_exact_audio(expected: &[f32], actual: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    assert!(expected.iter().any(|sample| sample.abs() > 1e-6));
    for (frame, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        assert!(expected.is_finite() && actual.is_finite());
        assert_eq!(actual.to_bits(), expected.to_bits(), "sample {frame}");
    }
}

fn impulse() -> DecodedSample {
    let pcm = (0..4096)
        .map(|index| ((index * 17 % 71) as f32 - 35.0) / 35.0)
        .collect();
    DecodedSample::from_parts(SAMPLE_RATE, 1, pcm).expect("custom impulse response")
}

fn params(custom: bool) -> ReverbParams {
    ReverbParams {
        size_secs: 0.2,
        fade_secs: 0.01,
        lp_start_hz: 8_000.0,
        lp_end_hz: 1_000.0,
        ir: custom.then_some(IrParams {
            sample: IR_SAMPLE,
            speed: 1.0,
            begin: 0.0,
        }),
    }
}

#[test]
fn portable_renderers_keep_their_selection_across_initialization() {
    let automatic = DspDispatch::automatic();
    assert!(!automatic.is_forced_portable());
    assert_eq!(
        automatic.convolution_kernel_kind(),
        rustel_audio::selected_convolution_kernel_kind()
    );
    assert_eq!(
        automatic.supersaw_kernel_kind(),
        rustel_audio::selected_supersaw_kernel_kind()
    );
    assert_eq!(
        automatic.wavetable_kernel_kind(),
        rustel_audio::selected_wavetable_kernel_kind()
    );

    let mut backend = ScalarBackend::with_dispatch(DspDispatch::portable());
    for rate in [24_000, 44_100] {
        backend.init(rate).expect("initialize portable backend");
        assert_portable(backend.dispatch());
    }
    let prepared = ScalarBackend::prepared_with_dispatch(SAMPLE_RATE, 1, DspDispatch::portable())
        .expect("prepared portable backend");
    assert_portable(prepared.dispatch());
    let live = LiveScalarBackend::with_dispatch(SAMPLE_RATE, 1, DspDispatch::portable())
        .expect("portable live backend");
    assert_portable(live.dispatch());
    assert!(!ScalarBackend::new().dispatch().is_forced_portable());
    assert!(!automatic.is_forced_portable());
}

fn render_reverb(reverb: &mut OrbitReverb, streaming: bool) -> Vec<f32> {
    let mut output = Vec::with_capacity(8192 * 2);
    let mut start = 0;
    while start < 8192 {
        let frames = if streaming { 1 } else { 128.min(8192 - start) };
        let mut input = [0.0; 128];
        for (index, sample) in input[..frames].iter_mut().enumerate() {
            let frame = start + index;
            if frame < 1024 {
                *sample = (frame % 29) as f32 / 29.0 - 0.5;
            }
        }
        let mut left = [0.0; 128];
        let mut right = [0.0; 128];
        reverb.process_block(
            &input[..frames],
            &input[..frames],
            &mut left[..frames],
            &mut right[..frames],
        );
        for (left, right) in left[..frames].iter().zip(&right[..frames]) {
            output.extend([*left, *right]);
        }
        start += frames;
    }
    output
}

#[test]
fn every_reverb_preparation_path_preserves_exact_portable_output() {
    let source = impulse();
    for mode in 0..3 {
        let prepare = |dispatch| match mode {
            0 => OrbitReverb::generate_with_dispatch(SAMPLE_RATE, params(false), dispatch),
            1 => {
                OrbitReverb::generate_streaming_with_dispatch(SAMPLE_RATE, params(false), dispatch)
            }
            _ => OrbitReverb::generate_custom_with_dispatch(
                SAMPLE_RATE,
                params(true),
                &source,
                dispatch,
            ),
        };
        let mut automatic = prepare(DspDispatch::automatic());
        let mut portable = prepare(DspDispatch::portable());
        assert_portable(portable.dispatch());
        let expected = render_reverb(&mut automatic, mode == 1);
        let actual = render_reverb(&mut portable, mode == 1);
        assert_exact_audio(&expected, &actual);
        portable.reset();
        assert_portable(portable.dispatch());
        assert_exact_audio(&actual, &render_reverb(&mut portable, mode == 1));
    }
}

fn render_voice(dispatch: DspDispatch, custom: bool) -> Vec<f32> {
    let mut backend = ScalarBackend::with_dispatch(dispatch);
    backend
        .install_sample(IR_SAMPLE, Box::new(impulse()))
        .expect("install IR");
    let params = params(custom);
    let room = ReverbControls {
        wet: 0.4,
        size_secs: params.size_secs,
        fade_secs: params.fade_secs,
        lp_start_hz: params.lp_start_hz,
        lp_end_hz: params.lp_end_hz,
        ir: params.ir,
    };
    let mut event = OnsetEvent::new(0, 220.0, 0.2, 0.1);
    event.synth = Some(SynthSource::Supersaw {
        voices: 8.0,
        freqspread: 0.35,
        panspread: 0.8,
    });
    event.controls.reverb = Some(room);
    event.controls.fx_stages[0] = Some(FxStage {
        stretch: None,
        transient: None,
        gain: 1.0,
        filters: Default::default(),
        vowel: None,
        coarse: None,
        crush: None,
        shape: None,
        distort: None,
        tremolo: None,
        compressor: None,
        pan_x: None,
        phaser: None,
        delay: None,
        dry: 1.0,
        room: Some(ReverbControls { ir: None, ..room }),
    });
    let pcm = render_pcm(&mut backend, SAMPLE_RATE, 16_384, &[event]).expect("complete render");
    assert_eq!(
        backend.dispatch().is_forced_portable(),
        dispatch.is_forced_portable()
    );
    pcm
}

#[test]
fn supersaw_and_inline_reverb_assets_match_the_automatic_render() {
    for custom in [false, true] {
        let automatic = render_voice(DspDispatch::automatic(), custom);
        let portable = render_voice(DspDispatch::portable(), custom);
        assert_exact_audio(&automatic, &portable);
        assert_exact_audio(&automatic, &render_voice(DspDispatch::automatic(), custom));
    }
}
