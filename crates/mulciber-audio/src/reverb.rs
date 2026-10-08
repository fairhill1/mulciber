//! Freeverb-style stereo reverb bus for the mixer: 8 parallel lowpass-comb filters and 4 series
//! allpasses per channel (the right channel detuned by a stereo spread), fed by a mono send
//! accumulated from the effect voices. Parameters arrive as targets and are smoothed per sample,
//! so the room can change while sounds play without zipper noise. All state lives on the audio
//! callback thread; the game side drives it through `Controls::set_room`, usually sized by
//! `probe_room`.

/// Comb and allpass delay lengths in samples at 44.1 kHz (the classic Freeverb tunings, mutually
/// detuned so the modes do not stack), scaled to the device rate at construction.
const COMB_TUNINGS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASS_TUNINGS: [usize; 4] = [556, 441, 341, 225];
/// Right-channel delay offset (samples at 44.1 kHz) that decorrelates the two tails into stereo
/// width.
const STEREO_SPREAD: usize = 23;
/// Send trim before the comb bank (Freeverb's fixed input gain).
const FIXED_GAIN: f32 = 0.015;
/// Tail output gain at wet = 1.
const SCALE_WET: f32 = 3.0;
/// `room` 0..1 maps to comb feedback 0.70..0.97: a tight slap for a burrow up to a many-second
/// bloom for a huge hall.
const FEEDBACK_LO: f32 = 0.70;
const FEEDBACK_SPAN: f32 = 0.27;
/// `damp` 0..1 maps to a 0..0.4 one-pole lowpass coefficient inside the comb feedback (higher is
/// darker, with faster-dying highs).
const SCALE_DAMP: f32 = 0.4;
/// Per-sample parameter smoothing time constant, seconds.
const SMOOTH_SECS: f32 = 0.05;
/// Below this the bus counts as dry and is skipped.
const SILENT_WET: f32 = 1.0e-4;

/// Kills numbers small enough to go denormal: a decaying comb tail would otherwise park the
/// audio thread in slow subnormal arithmetic.
#[inline]
fn flush(value: f32) -> f32 {
    if value.abs() < 1.0e-18 { 0.0 } else { value }
}

/// Feedback comb with a one-pole lowpass in the loop (Freeverb's LBCF).
struct Comb {
    buffer: Vec<f32>,
    cursor: usize,
    filter: f32,
}

impl Comb {
    fn new(length: usize) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            cursor: 0,
            filter: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let output = self.buffer[self.cursor];
        self.filter = flush(output * (1.0 - damp) + self.filter * damp);
        self.buffer[self.cursor] = flush(input + self.filter * feedback);
        self.cursor += 1;
        if self.cursor >= self.buffer.len() {
            self.cursor = 0;
        }
        output
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.filter = 0.0;
    }
}

/// Schroeder allpass with a fixed 0.5 coefficient: smears the comb echoes into a diffuse tail
/// without colouring its decay.
struct AllPass {
    buffer: Vec<f32>,
    cursor: usize,
}

impl AllPass {
    fn new(length: usize) -> Self {
        Self {
            buffer: vec![0.0; length.max(1)],
            cursor: 0,
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.cursor];
        self.buffer[self.cursor] = flush(input + delayed * 0.5);
        self.cursor += 1;
        if self.cursor >= self.buffer.len() {
            self.cursor = 0;
        }
        delayed - input
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
    }
}

pub(crate) struct Reverb {
    combs_left: Vec<Comb>,
    combs_right: Vec<Comb>,
    allpasses_left: Vec<AllPass>,
    allpasses_right: Vec<AllPass>,
    /// Smoothed live parameters: what the DSP uses this sample.
    wet: f32,
    feedback: f32,
    damp: f32,
    /// Targets from the last `set_params`.
    target_wet: f32,
    target_feedback: f32,
    target_damp: f32,
    /// One-pole smoothing coefficient toward the targets, per sample.
    smoothing: f32,
    /// The delay lines hold stale energy after the bus goes idle; they are cleared once on the
    /// idle edge so entering the next room does not replay old sounds.
    dirty: bool,
}

impl Reverb {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let rate = sample_rate as usize;
        let scale = |length: usize| (length * rate / 44_100).max(1);
        #[allow(clippy::cast_precision_loss)]
        let smoothing = 1.0 - (-1.0 / (SMOOTH_SECS * sample_rate as f32)).exp();
        Self {
            combs_left: COMB_TUNINGS.iter().map(|&v| Comb::new(scale(v))).collect(),
            combs_right: COMB_TUNINGS
                .iter()
                .map(|&v| Comb::new(scale(v + STEREO_SPREAD)))
                .collect(),
            allpasses_left: ALLPASS_TUNINGS
                .iter()
                .map(|&v| AllPass::new(scale(v)))
                .collect(),
            allpasses_right: ALLPASS_TUNINGS
                .iter()
                .map(|&v| AllPass::new(scale(v + STEREO_SPREAD)))
                .collect(),
            wet: 0.0,
            feedback: FEEDBACK_LO,
            damp: 0.5 * SCALE_DAMP,
            target_wet: 0.0,
            target_feedback: FEEDBACK_LO,
            target_damp: 0.5 * SCALE_DAMP,
            smoothing,
            dirty: false,
        }
    }

    /// All inputs 0..1: `wet` is the tail level, `room` its decay length and `damp` the
    /// high-frequency absorption.
    pub(crate) fn set_params(&mut self, wet: f32, room: f32, damp: f32) {
        self.target_wet = wet.clamp(0.0, 1.0);
        self.target_feedback = FEEDBACK_LO + FEEDBACK_SPAN * room.clamp(0.0, 1.0);
        self.target_damp = SCALE_DAMP * damp.clamp(0.0, 1.0);
    }

    /// Runs the mono send through the tank and adds the stereo tail into the interleaved
    /// output: `send.len()` frames, `output.len()` = frames × channels. A cheap no-op while the
    /// bus is fully dry.
    pub(crate) fn process(&mut self, send: &[f32], output: &mut [f32], channels: usize) {
        if self.wet < SILENT_WET && self.target_wet < SILENT_WET {
            if self.dirty {
                for comb in self.combs_left.iter_mut().chain(&mut self.combs_right) {
                    comb.clear();
                }
                for allpass in self
                    .allpasses_left
                    .iter_mut()
                    .chain(&mut self.allpasses_right)
                {
                    allpass.clear();
                }
                self.wet = 0.0;
                self.dirty = false;
            }
            return;
        }
        self.dirty = true;

        for (frame, &sample) in send.iter().enumerate() {
            self.wet += (self.target_wet - self.wet) * self.smoothing;
            self.feedback += (self.target_feedback - self.feedback) * self.smoothing;
            self.damp += (self.target_damp - self.damp) * self.smoothing;

            let input = sample * FIXED_GAIN;
            let mut left = 0.0;
            let mut right = 0.0;
            for comb in &mut self.combs_left {
                left += comb.process(input, self.feedback, self.damp);
            }
            for comb in &mut self.combs_right {
                right += comb.process(input, self.feedback, self.damp);
            }
            for allpass in &mut self.allpasses_left {
                left = allpass.process(left);
            }
            for allpass in &mut self.allpasses_right {
                right = allpass.process(right);
            }
            let gain = self.wet * SCALE_WET;
            left *= gain;
            right *= gain;

            if channels >= 2 {
                let base = frame * channels;
                output[base] += left;
                output[base + 1] += right;
                // Extra channels get the same centre feed as the dry mix.
                let center = f32::midpoint(left, right);
                for sample in &mut output[base + 2..base + channels] {
                    *sample += center;
                }
            } else {
                output[frame] += f32::midpoint(left, right);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(buffer: &[f32]) -> f32 {
        buffer.iter().map(|sample| sample * sample).sum()
    }

    /// Feeds one impulse after the parameters have settled and returns the tail energy in each
    /// following 100 ms window.
    fn impulse_windows(wet: f32, room: f32, damp: f32, windows: usize) -> Vec<f32> {
        let mut reverb = Reverb::new(48_000);
        reverb.set_params(wet, room, damp);
        let silent = vec![0.0; 4_800];
        let mut output = vec![0.0; 9_600];
        reverb.process(&silent, &mut output, 2);

        let mut impulse = vec![0.0; 4_800];
        impulse[0] = 1.0;
        output.fill(0.0);
        reverb.process(&impulse, &mut output, 2);
        let mut energies = vec![energy(&output)];
        for _ in 1..windows {
            output.fill(0.0);
            reverb.process(&silent, &mut output, 2);
            assert!(output.iter().all(|sample| sample.is_finite()));
            energies.push(energy(&output));
        }
        energies
    }

    /// An impulse through a big room rings, stays finite, is still audible half a second later,
    /// and decays overall.
    #[test]
    fn tail_rings_and_decays() {
        let energies = impulse_windows(1.0, 0.8, 0.3, 21);
        assert!(energies[0] > 0.0, "the impulse excites the tail");
        assert!(energies[5] > 0.0, "the tail still rings at 500 ms");
        assert!(
            energies[20] < energies[0],
            "the tail decays over two seconds"
        );
    }

    #[test]
    fn a_bigger_room_rings_longer() {
        let small = impulse_windows(1.0, 0.1, 0.5, 10);
        let large = impulse_windows(1.0, 0.95, 0.5, 10);
        let late = |energies: &[f32]| energies[5..].iter().sum::<f32>() / energies[0];
        assert!(
            late(&large) > 10.0 * late(&small),
            "late energy ratio {} vs {}",
            late(&large),
            late(&small)
        );
    }

    #[test]
    fn wet_scales_the_tail_level() {
        let quiet = impulse_windows(0.2, 0.6, 0.5, 3);
        let loud = impulse_windows(0.8, 0.6, 0.5, 3);
        let ratio = loud[1] / quiet[1];
        assert!((ratio - 16.0).abs() < 0.5, "energy ratio {ratio}");
    }

    #[test]
    fn damping_dulls_the_tail() {
        let bright = impulse_windows(1.0, 0.8, 0.0, 10);
        let dark = impulse_windows(1.0, 0.8, 1.0, 10);
        assert!(dark[9] < bright[9]);
    }

    /// A fully dry bus leaves the output untouched (the idle early-out).
    #[test]
    fn dry_when_wet_is_zero() {
        let mut reverb = Reverb::new(48_000);
        reverb.set_params(0.0, 0.5, 0.5);
        let mut send = vec![0.0; 1_024];
        send[0] = 1.0;
        let mut output = vec![0.0; 2_048];
        reverb.process(&send, &mut output, 2);
        assert!(output.iter().all(|&sample| sample == 0.0));
    }

    /// Drying the bus clears the delay lines, so the next room starts silent.
    #[test]
    fn going_dry_forgets_the_old_room() {
        let mut reverb = Reverb::new(48_000);
        reverb.set_params(1.0, 0.9, 0.2);
        let mut send = vec![0.0; 4_800];
        send[0] = 1.0;
        let mut output = vec![0.0; 9_600];
        reverb.process(&send, &mut output, 2);
        reverb.set_params(0.0, 0.9, 0.2);
        let silent = vec![0.0; 4_800];
        for _ in 0..10 {
            reverb.process(&silent, &mut output, 2);
        }
        reverb.set_params(1.0, 0.9, 0.2);
        output.fill(0.0);
        reverb.process(&silent, &mut output, 2);
        assert!(output.iter().all(|&sample| sample == 0.0));
    }
}
