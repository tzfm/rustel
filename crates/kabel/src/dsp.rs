// DSP formulas follow KabelSalat's lib/ugens.js and lib/synth.js. See NOTICE.
use super::{Node, Op, js_max, js_min, js_round, truthy};
use std::f64::consts::TAU;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct State {
    v: [f64; 12],
    index: usize,
    random: u32,
    flags: u8,
    pub(super) boolean: bool,
}
impl State {
    pub(super) fn for_node(node: Node, index: usize) -> Self {
        let mut state = Self {
            random: 340_u32.wrapping_mul(u32::from(node.auxiliary) + 1),
            ..Self::default()
        };
        match node.op {
            Op::Lcgnoise => state.v[1] = f64::from(state.random),
            Op::Impulse => state.v[0] = 1.0,
            Op::Clockdiv => state.flags = 3,
            Op::Seq => state.flags = 1,
            Op::Saw | Op::Noise | Op::Pink | Op::Brown | Op::Dust => {
                state.random = (index as u32)
                    .wrapping_mul(747796405)
                    .wrapping_add(2891336453);
                if node.op == Op::Saw {
                    state.v[0] = state.random();
                }
                if node.op == Op::Noise {
                    state.v[0] = state.random() * 2.0 - 1.0;
                }
            }
            Op::Qf => {
                state.v[4] = 1.0;
            }
            _ => {}
        }
        state
    }
    fn random(&mut self) -> f64 {
        self.random = self.random.wrapping_mul(1664525).wrapping_add(1013904223);
        f64::from(self.random) / 4294967296.0
    }
    pub(super) fn process(
        &mut self,
        op: Op,
        count: usize,
        get: &impl Fn(usize, f64) -> f64,
        get_boolean: &impl Fn(usize) -> bool,
        sample_rate: f64,
        time: f64,
    ) -> f64 {
        if op != Op::Hold {
            self.boolean = false;
        }
        let a = get(0, 0.0);
        let b = get(1, 0.0);
        let dt = 1.0 / sample_rate;
        match op {
            Op::Adsr => self.envelope(time, a, get(1, 0.02), get(2, 0.1), get(3, 0.2), get(4, 0.1)),
            Op::Clock => {
                self.v[0] += dt * (24.0 * get(0, 120.0) / 60.0);
                if self.v[0] % 1.0 < 0.5 { 1.0 } else { -1.0 }
            }
            Op::Clockdiv => {
                let positive = a > 0.0;
                if positive != (self.flags & 1 != 0) {
                    self.index = self.index.saturating_add(1);
                    if self.index as f64 >= get(1, 2.0) {
                        self.index = 0;
                        self.flags ^= 2;
                    }
                }
                self.flags = (self.flags & !1) | u8::from(positive);
                if self.flags & 2 != 0 { 1.0 } else { -1.0 }
            }
            Op::Distort => {
                let amount = b.clamp(0.0, 1.0) - 0.01;
                let k = 2.0 * amount / (1.0 - amount);
                (1.0 + k) * a / (1.0 + k * a.abs())
            }
            Op::Noise => {
                if truthy(get(0, 1.0)) {
                    self.v[0] = self.random() * 2.0 - 1.0;
                }
                self.v[0]
            }
            Op::Lcgnoise => {
                if !truthy(get(0, 1.0)) {
                    return self.v[0];
                }
                if truthy(b) {
                    self.random = self.v[1] as u32;
                }
                self.random = self.random.wrapping_mul(1664525).wrapping_add(1013904223);
                self.v[0] = 2.0 * f64::from(self.random & 0xffffff) / 16777216.0 - 1.0;
                self.v[0]
            }
            Op::Dust => {
                if self.random() < a / 48000.0 {
                    self.random()
                } else {
                    0.0
                }
            }
            Op::Brown => {
                let white = self.random() * 2.0 - 1.0;
                self.v[0] = (self.v[0] + 0.02 * white) / 1.02;
                self.v[0]
            }
            Op::Pink => {
                let white = self.random() * 2.0 - 1.0;
                self.v[0] = 0.99886 * self.v[0] + white * 0.0555179;
                self.v[1] = 0.99332 * self.v[1] + white * 0.0750759;
                self.v[2] = 0.96900 * self.v[2] + white * 0.1538520;
                self.v[3] = 0.86650 * self.v[3] + white * 0.3104856;
                self.v[4] = 0.55000 * self.v[4] + white * 0.5329522;
                self.v[5] = -0.7616 * self.v[5] - white * 0.0168980;
                let pink = self.v[..7].iter().sum::<f64>() + white * 0.5362;
                self.v[6] = white * 0.115926;
                pink * 0.11
            }
            Op::Impulse => {
                self.v[0] += dt * a;
                let output = f64::from(self.v[0] >= 1.0);
                self.v[0] %= 1.0;
                output
            }
            Op::Saw => {
                let delta = a * dt;
                let output = 2.0 * self.v[0] - 1.0 - poly_blep(self.v[0], delta);
                self.v[0] += delta;
                if self.v[0] > 1.0 {
                    self.v[0] -= 1.0;
                }
                output
            }
            Op::Zaw => {
                self.v[0] += dt * a;
                (self.v[0] % 1.0) * 2.0 - 1.0
            }
            Op::Sine => {
                let sync = b > 0.0;
                if sync && self.flags == 0 {
                    self.v[0] = 0.0;
                }
                self.flags = u8::from(sync);
                let cycle = (self.v[0] + get(2, 0.0)) % 1.0;
                self.v[0] += dt * a;
                (cycle * TAU).sin()
            }
            Op::Tri => {
                self.v[0] += dt * a;
                let cycle = self.v[0] % 1.0;
                let norm = if cycle < 0.5 {
                    2.0 * cycle
                } else {
                    1.0 - 2.0 * (cycle - 0.5)
                };
                norm * 2.0 - 1.0
            }
            Op::Pulse => {
                self.v[0] += dt * a;
                if self.v[0] % 1.0 < get(1, 0.5) {
                    1.0
                } else {
                    -1.0
                }
            }
            Op::Slide | Op::Lag => {
                let mut rate = get(1, 1.0) * if op == Op::Lag { 4410.0 } else { 1000.0 };
                if rate < 1.0 {
                    rate = 1.0;
                }
                self.v[0] += (1.0 / rate) * (a - self.v[0]);
                self.v[0]
            }
            Op::Slew => {
                let mut delta = a - self.v[0];
                let up = get(1, 1.0) * (1.0 / 48000.0);
                let down = get(2, 1.0) * (1.0 / 48000.0);
                if delta > up {
                    delta = up;
                } else if delta < -down {
                    delta = -down;
                }
                self.v[0] += delta;
                self.v[0]
            }
            Op::Filter | Op::Bpf => {
                let cutoff = js_min(get(1, 1.0), 1.0);
                let resonance = js_max(get(2, 0.0), 0.0);
                let c = 0.5_f64.powf((1.0 - cutoff) / 0.125);
                let r = 0.5_f64.powf((resonance + 0.125) / 0.125);
                let mrc = 1.0 - r * c;
                let v0 = mrc * self.v[0] - c * self.v[1] + c * a;
                let v1 = mrc * self.v[1] + c * v0;
                self.v[0] = v0;
                self.v[1] = v1;
                if op == Op::Filter { v1 } else { v0 }
            }
            Op::Fold => {
                let rate = if b < 0.0 { 0.0 } else { b };
                let value = 0.25 * (a * (rate + 1.0)) + 0.25;
                4.0 * ((value - js_round(value)).abs() - 0.25)
            }
            Op::Seq => {
                let positive = a > 0.0;
                let rising = positive && self.flags == 0;
                self.flags = u8::from(positive);
                let length = count.saturating_sub(1);
                if rising {
                    if length > 0 {
                        self.index = (self.index + 1) % length;
                    }
                    return 0.0;
                }
                if length == 0 {
                    return f64::NAN;
                }
                self.boolean = get_boolean(1 + self.index % length);
                get(1 + self.index % length, 0.0)
            }
            Op::Hold => {
                let positive = b > 0.0;
                if positive && self.flags == 0 {
                    self.v[0] = a;
                    self.boolean = get_boolean(0);
                }
                self.flags = u8::from(positive);
                self.v[0]
            }
            Op::Pick => {
                let length = count.saturating_sub(1);
                if length == 0 || !a.is_finite() {
                    return f64::NAN;
                }
                let index = ((a % length as f64 + length as f64) % length as f64).floor() as usize;
                self.boolean = get_boolean(1 + index % length);
                get(1 + index % length, 0.0)
            }
            Op::Trig => {
                if a > 0.0 && self.flags == 0 {
                    self.flags = 1;
                    return 1.0;
                }
                if a <= 0.0 {
                    self.flags = 0;
                }
                0.0
            }
            Op::Qf => self.biquad(
                a,
                if get_boolean(1) { f64::NAN } else { b },
                get(2, 500.0),
                get(3, 1.0),
                get(4, 1.0),
                sample_rate,
            ),
            _ => 0.0,
        }
    }
    fn envelope(
        &mut self,
        time: f64,
        gate: f64,
        attack: f64,
        decay: f64,
        sustain: f64,
        release: f64,
    ) -> f64 {
        let elapsed = time - self.v[0];
        match self.flags {
            0 => {
                if gate > 0.0 {
                    self.flags = 1;
                    self.v[0] = time;
                    self.v[1] = 0.0;
                }
                0.0
            }
            1 => {
                if elapsed > attack {
                    self.flags = 2;
                    self.v[0] = time;
                    1.0
                } else {
                    envelope_lerp(elapsed / attack, self.v[1], 1.0)
                }
            }
            2 => {
                let current = envelope_lerp(elapsed / decay, 1.0, sustain);
                if gate <= 0.0 {
                    self.flags = 4;
                    self.v[0] = time;
                    self.v[1] = current;
                    return current;
                }
                if elapsed > decay {
                    self.flags = 3;
                    self.v[0] = time;
                    sustain
                } else {
                    current
                }
            }
            3 => {
                if gate <= 0.0 {
                    self.flags = 4;
                    self.v[0] = time;
                    self.v[1] = sustain;
                }
                sustain
            }
            _ => {
                if elapsed > release {
                    self.flags = 0;
                    return 0.0;
                }
                let current = envelope_lerp(elapsed / release, self.v[1], 0.0);
                if gate > 0.0 {
                    self.flags = 1;
                    self.v[0] = time;
                    self.v[1] = current;
                }
                current
            }
        }
    }
    fn biquad(
        &mut self,
        input: f64,
        kind: f64,
        frequency: f64,
        q: f64,
        gain: f64,
        sample_rate: f64,
    ) -> f64 {
        let omega = TAU * frequency / sample_rate;
        let sin = omega.sin();
        let cos = omega.cos();
        let alpha = sin / (2.0 * 10.0_f64.powf(q / 20.0));
        let a = 10.0_f64.powf(gain / 40.0);
        let coefficients = match kind {
            0.0 => Some([
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ]),
            1.0 => Some([
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ]),
            2.0 => Some([
                sin / 2.0,
                0.0,
                -sin / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ]),
            3.0 => Some([1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha]),
            4.0 => Some([
                1.0 - alpha,
                -2.0 * cos,
                1.0 + alpha,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ]),
            5.0 => Some([
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ]),
            6.0 | 7.0 => {
                let sqrt = 2.0 * a.sqrt() * alpha;
                let am = (a - 1.0) * cos;
                let ap = (a + 1.0) * cos;
                if kind == 6.0 {
                    Some([
                        a * (a + 1.0 - am + sqrt),
                        2.0 * a * (a - 1.0 - ap),
                        a * (a + 1.0 - am - sqrt),
                        a + 1.0 + am + sqrt,
                        -2.0 * (a - 1.0 + ap),
                        a + 1.0 + am - sqrt,
                    ])
                } else {
                    Some([
                        a * (a + 1.0 + am + sqrt),
                        -2.0 * a * (a - 1.0 + ap),
                        a * (a + 1.0 + am - sqrt),
                        a + 1.0 - am + sqrt,
                        2.0 * (a - 1.0 - ap),
                        a + 1.0 - am - sqrt,
                    ])
                }
            }
            _ => None,
        };
        if let Some(c) = coefficients {
            self.v[4] = c[0] / c[3];
            self.v[5] = c[1] / c[3];
            self.v[6] = c[2] / c[3];
            self.v[7] = c[4] / c[3];
            self.v[8] = c[5] / c[3];
        }
        let output = self.v[4] * input + self.v[5] * self.v[0] + self.v[6] * self.v[1]
            - self.v[7] * self.v[2]
            - self.v[8] * self.v[3];
        self.v[1] = self.v[0];
        self.v[0] = input;
        self.v[3] = self.v[2];
        self.v[2] = output;
        output
    }
}
#[derive(Debug)]
pub(super) struct Delay {
    buffer: Vec<f32>,
    position: usize,
    written: usize,
}
impl Delay {
    pub(super) fn new(length: usize) -> Result<Self, String> {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(length)
            .map_err(|_| "could not allocate KabelSalat delay memory")?;
        buffer.resize(length, 0.0);
        Ok(Self {
            buffer,
            position: 0,
            written: 0,
        })
    }
    pub(super) fn heap_bytes(&self) -> usize {
        self.buffer.capacity() * std::mem::size_of::<f32>()
    }
    pub(super) fn reset(&mut self) {
        self.position = 0;
        self.written = 0;
    }
    pub(super) fn process(&mut self, input: f64, time: f64, sample_rate: f64) -> f64 {
        self.position = (self.position + 1) % self.buffer.len();
        self.buffer[self.position] = input as f32;
        self.written = (self.written + 1).min(self.buffer.len());
        let samples = js_min((sample_rate * time).floor(), (self.buffer.len() - 1) as f64);
        let mut read = self.position as f64 - samples;
        if read < 0.0 {
            read += self.buffer.len() as f64;
        }
        if !read.is_finite() || read < 0.0 || read >= self.buffer.len() as f64 {
            return f64::NAN;
        }
        let read = read as usize;
        // Before the first wrap only slots 1..=position belong to this voice.
        if self.written < self.buffer.len() && (read == 0 || read > self.position) {
            return 0.0;
        }
        f64::from(self.buffer[read])
    }
}
fn envelope_lerp(value: f64, lo: f64, hi: f64) -> f64 {
    if value >= 1.0 {
        hi
    } else {
        lo + value * (hi - lo)
    }
}
fn lerp(value: f64, lo: f64, hi: f64) -> f64 {
    if value <= 0.0 {
        return lo;
    }
    if value >= 1.0 {
        return hi;
    }
    lo + value * (hi - lo)
}
pub(super) fn remap(value: f64, a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    let normalized = if value <= a0 {
        0.0
    } else if value >= a1 {
        1.0
    } else if a1 == a0 {
        0.0
    } else {
        (value - a0) / (a1 - a0)
    };
    lerp(normalized, b0, b1)
}
fn poly_blep(mut phase: f64, delta: f64) -> f64 {
    if phase < delta {
        phase /= delta;
        phase + phase - phase * phase - 1.0
    } else if phase > 1.0 - delta {
        phase = (phase - 1.0) / delta;
        phase * phase + phase + phase + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Program;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Fixture {
        kind: String,
        rate: f64,
        inputs: Vec<Vec<f64>>,
        time: Vec<f64>,
        expected: Vec<f64>,
    }

    #[test]
    fn matches_upstream_ugen_numerical_vectors() {
        let fixtures: Vec<Fixture> =
            serde_json::from_str(include_str!("../tests/fixtures/ugens.json")).unwrap();
        for fixture in fixtures {
            let program =
                Program::from_json(&format!(r#"{{"nodes":[{{"type":"{}"}}]}}"#, fixture.kind))
                    .unwrap();
            let node = program.nodes[0];
            let mut state = State::for_node(node, 0);
            let mut delay = Delay::new((fixture.rate * 10.0) as usize).unwrap();
            for (index, args) in fixture.inputs.iter().enumerate() {
                let get = |i: usize, default: f64| args.get(i).copied().unwrap_or(default);
                let result = match node.op {
                    Op::Delay => delay.process(get(0, 0.0), get(1, 0.0), fixture.rate),
                    Op::Remap => remap(
                        get(0, 0.0),
                        get(1, -1.0),
                        get(2, 1.0),
                        get(3, -1.0),
                        get(4, 1.0),
                    ),
                    Op::Clip => get(0, 0.0).max(get(1, -1.0)).min(get(2, 1.0)),
                    _ => state.process(
                        node.op,
                        args.len(),
                        &get,
                        &|_| false,
                        fixture.rate,
                        fixture.time[index],
                    ),
                };
                let expected = fixture.expected[index];
                assert!(
                    (result - expected).abs() <= 2e-12,
                    "{} sample {index}: {result} != {expected}",
                    fixture.kind
                );
            }
        }
    }
}
