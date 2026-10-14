//! Live audio: a lock-free ring from the game's thread to the callback's, the jitter buffer that
//! plays a stream through the mixer, and a resampler that keeps its place between chunks.
//!
//! A stream's samples arrive in bursts (a network packet every 20 ms, a decoder's frame) and on a
//! clock of their own (another machine's microphone), so the voice that plays one holds a little
//! audio back, [`StreamConfig::latency`] of it, and plays a hair faster or slower to keep that
//! much in hand: at most 1% either way, which speech doesn't show, and enough for any two sound
//! cards' clocks. When it runs dry it fades out and waits to fill again; when it has fallen far
//! behind ([`StreamConfig::max_latency`]) it fades out, drops what's late, and fades back in.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::mixer::VoiceId;

/// How a live stream is buffered: its rate and how much of it is held back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamConfig {
    /// The rate of the samples pushed, in frames per second. The voice resamples to the mixer's.
    pub sample_rate: u32,
    /// The audio held back to ride out late and bunched samples, in seconds. The voice starts
    /// once it has this much, and plays faster or slower to keep it.
    pub latency: f32,
    /// How far behind the voice may fall, in seconds, before it drops what's late to catch up to
    /// [`Self::latency`].
    pub max_latency: f32,
}

impl StreamConfig {
    /// A stream at `sample_rate` holding back 60 ms, three 20 ms packets, and catching up from
    /// 250 ms behind.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            latency: 0.06,
            max_latency: 0.25,
        }
    }
}

/// A single-producer, single-consumer ring of `f32` samples, stored as their bits so neither side
/// needs a lock or `unsafe`. The producer publishes with a release store of `write` and the
/// consumer frees with one of `read`, so each side sees the other's samples whole.
#[derive(Debug)]
pub(crate) struct Ring {
    slots: Box<[AtomicU32]>,
    mask: usize,
    write: AtomicUsize,
    read: AtomicUsize,
    /// The producer is gone: nothing more will come.
    closed: AtomicBool,
    /// The consumer is gone: nothing pushed will be heard.
    abandoned: AtomicBool,
}

impl Ring {
    /// A ring holding at least `capacity` samples.
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        Self {
            slots: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            mask: capacity - 1,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
        }
    }

    pub(crate) fn len(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        self.write.load(Ordering::Acquire).wrapping_sub(read)
    }

    /// Producer side: copies in what fits and returns how many were taken.
    pub(crate) fn push(&self, samples: &[f32]) -> usize {
        let write = self.write.load(Ordering::Relaxed);
        let free = self.slots.len() - write.wrapping_sub(self.read.load(Ordering::Acquire));
        let taken = samples.len().min(free);
        for (offset, sample) in samples[..taken].iter().enumerate() {
            self.slots[write.wrapping_add(offset) & self.mask]
                .store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write
            .store(write.wrapping_add(taken), Ordering::Release);
        taken
    }

    /// Consumer side: the oldest sample, if any.
    pub(crate) fn pop(&self) -> Option<f32> {
        let read = self.read.load(Ordering::Relaxed);
        if self.write.load(Ordering::Acquire) == read {
            return None;
        }
        let sample = f32::from_bits(self.slots[read & self.mask].load(Ordering::Relaxed));
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(sample)
    }

    /// Consumer side: drops up to `count` of the oldest samples.
    pub(crate) fn discard(&self, count: usize) {
        let read = self.read.load(Ordering::Relaxed);
        let available = self.write.load(Ordering::Acquire).wrapping_sub(read);
        self.read
            .store(read.wrapping_add(count.min(available)), Ordering::Release);
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn abandon(&self) {
        self.abandoned.store(true, Ordering::Release);
    }

    pub(crate) fn is_abandoned(&self) -> bool {
        self.abandoned.load(Ordering::Acquire)
    }
}

/// The game's end of a live voice started with [`Controls::play_spatial_stream`]: push mono
/// samples at its [`StreamConfig::sample_rate`] as they come.
///
/// Dropping it ends the voice once what was pushed has played. [`Controls::stop_voice`] with
/// [`Self::id`] fades it out at once, as for any voice.
///
/// [`Controls::play_spatial_stream`]: crate::Controls::play_spatial_stream
/// [`Controls::stop_voice`]: crate::Controls::stop_voice
#[derive(Debug)]
pub struct VoiceStream {
    pub(crate) ring: Arc<Ring>,
    pub(crate) id: VoiceId,
    pub(crate) sample_rate: u32,
}

impl VoiceStream {
    /// Queues mono samples to play, returning how many were taken: fewer only when the voice has
    /// fallen twice [`StreamConfig::max_latency`] behind, its mixer stalled, or it has ended.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        self.ring.push(samples)
    }

    /// The voice, for [`crate::Controls::stop_voice`] and [`crate::Controls::set_voice_gain`].
    #[must_use]
    pub fn id(&self) -> VoiceId {
        self.id
    }

    /// Whether the voice still plays: `false` once it was stopped, its emitter was, or the
    /// mixer is gone, after which pushing is pointless and a new voice is needed.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        !self.ring.is_abandoned()
    }

    /// The audio queued and not yet played, in seconds.
    #[must_use]
    pub fn buffered(&self) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let seconds = self.ring.len() as f32 / self.sample_rate.max(1) as f32;
        seconds
    }
}

impl Drop for VoiceStream {
    fn drop(&mut self) {
        self.ring.close();
    }
}

/// How far the playback rate leans to keep the buffer at its latency: 1% for each latency's worth
/// over or under, ...
const LEAN_PER_LATENCY: f64 = 0.01;
/// ... and never more than 1%.
const MAX_LEAN: f64 = 0.01;
/// How long the buffer's fill is averaged over before the rate leans on it, in seconds: long
/// enough that a packet landing doesn't wobble the pitch.
const FILL_SMOOTHING: f64 = 0.5;
/// The fades that keep a stream starting, running dry and skipping from clicking, in seconds.
const DECLICK: f32 = 0.0025;

/// The mixer's end of a live voice: the jitter buffer and drift control over a [`Ring`].
#[derive(Debug)]
pub(crate) struct StreamReader {
    ring: Arc<Ring>,
    /// Source frames per output frame at the nominal rates.
    step: f64,
    /// The fill held, and the fill from which the voice catches up, in source samples.
    target: usize,
    max: usize,
    /// Output frames a part-filled buffer waits for more before playing what it has.
    patience: usize,
    smoothing: f64,
    fill: f64,
    window: [f32; 4],
    fraction: f64,
    playing: bool,
    /// The fade envelope and how far it moves per output frame.
    envelope: f32,
    envelope_step: f32,
    /// Fading out to drop what's late.
    catching_up: bool,
    /// The last sample, easing to silence after the buffer ran dry.
    tail: f32,
    waited: usize,
    waited_at: usize,
}

impl StreamReader {
    /// The reader and its writer for `config`, played by a mixer at `output_rate`.
    pub(crate) fn pair(config: StreamConfig, output_rate: u32, id: VoiceId) -> (Self, VoiceStream) {
        let source_rate = f64::from(config.sample_rate.max(1));
        let output = f64::from(output_rate.max(1));
        let seconds = |seconds: f32| -> usize {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let samples = (f64::from(seconds.max(0.0)) * source_rate).round() as usize;
            samples
        };
        let target = seconds(config.latency).max(1);
        let max = seconds(config.max_latency).max(target * 2);
        let ring = Arc::new(Ring::new(max * 2));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let patience = (f64::from(config.latency.max(0.0)) * output) as usize;
        #[allow(clippy::cast_possible_truncation)]
        let envelope_step = (1.0 / (f64::from(DECLICK) * output).max(1.0)) as f32;
        let reader = Self {
            ring: Arc::clone(&ring),
            step: source_rate / output,
            target,
            max,
            patience: patience.max(1),
            smoothing: 1.0 / (FILL_SMOOTHING * output),
            fill: 0.0,
            window: [0.0; 4],
            fraction: 0.0,
            playing: false,
            envelope: 0.0,
            envelope_step,
            catching_up: false,
            tail: 0.0,
            waited: 0,
            waited_at: 0,
        };
        let writer = VoiceStream {
            ring,
            id,
            sample_rate: config.sample_rate,
        };
        (reader, writer)
    }

    /// The next output frame, or `None` once the writer is gone and everything has played.
    pub(crate) fn next(&mut self) -> Option<f32> {
        let fill = self.ring.len();
        if !self.playing && !self.start(fill) {
            return self.idle();
        }
        #[allow(clippy::cast_precision_loss)]
        let fill_now = fill as f64;
        self.fill += (fill_now - self.fill) * self.smoothing;
        if fill > self.max {
            self.catching_up = true;
        }
        if self.catching_up {
            self.envelope = (self.envelope - self.envelope_step).max(0.0);
            if self.envelope == 0.0 {
                self.ring
                    .discard(self.ring.len().saturating_sub(self.target));
                self.prime();
                self.catching_up = false;
            }
        } else {
            self.envelope = (self.envelope + self.envelope_step).min(1.0);
        }
        let sample = hermite(self.window, self.fraction) * self.envelope;
        #[allow(clippy::cast_precision_loss)]
        let error = (self.fill - self.target as f64) / self.target as f64;
        self.fraction += self.step * (1.0 + (error * LEAN_PER_LATENCY).clamp(-MAX_LEAN, MAX_LEAN));
        while self.fraction >= 1.0 {
            self.fraction -= 1.0;
            let Some(next) = self.ring.pop() else {
                self.playing = false;
                self.tail = sample;
                self.waited = 0;
                break;
            };
            self.window = [self.window[1], self.window[2], self.window[3], next];
        }
        Some(sample)
    }

    /// Starts playing when the buffer holds its latency, or less when the writer is done or has
    /// gone quiet with a short phrase in it.
    fn start(&mut self, fill: usize) -> bool {
        if fill == 0 {
            self.waited = 0;
            return false;
        }
        if fill != self.waited_at {
            self.waited = 0;
            self.waited_at = fill;
        }
        if fill < self.target && !self.ring.is_closed() && self.waited < self.patience {
            self.waited += 1;
            return false;
        }
        self.prime();
        self.playing = true;
        self.envelope = 0.0;
        #[allow(clippy::cast_precision_loss)]
        let fill = fill as f64;
        self.fill = fill;
        true
    }

    /// Fills the interpolation window from the ring, the first sample doubled as its history.
    fn prime(&mut self) {
        let first = self.ring.pop().unwrap_or(0.0);
        let second = self.ring.pop().unwrap_or(first);
        let third = self.ring.pop().unwrap_or(second);
        self.window = [first, first, second, third];
        self.fraction = 0.0;
    }

    /// A frame while waiting for samples: the last one easing to silence, then silence, or the
    /// end once the writer is gone.
    fn idle(&mut self) -> Option<f32> {
        if self.tail != 0.0 {
            let step = self.envelope_step;
            self.tail = if self.tail.abs() <= step {
                0.0
            } else {
                self.tail - step * self.tail.signum()
            };
            return Some(self.tail);
        }
        if self.ring.is_closed() && self.ring.len() == 0 {
            return None;
        }
        Some(0.0)
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.ring.abandon();
    }
}

/// Four-point, third-order Hermite interpolation between `window[1]` and `window[2]`. Smoother
/// than a straight line between them, so resampling doesn't dull speech or fold much of its top
/// back down.
fn hermite(window: [f32; 4], fraction: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let t = fraction as f32;
    let [y0, y1, y2, y3] = window;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// A resampler for audio that arrives in chunks, a microphone's or a decoder's: it keeps its
/// place and its last samples between them, so the chunks join without a seam. Mono, by
/// four-point Hermite interpolation.
#[derive(Clone, Debug)]
pub struct StreamResampler {
    step: f64,
    window: [f32; 4],
    fraction: f64,
    primed: usize,
}

impl StreamResampler {
    /// A resampler from `from` to `to` frames per second.
    #[must_use]
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from.max(1)) / f64::from(to.max(1)),
            window: [0.0; 4],
            fraction: 0.0,
            primed: 0,
        }
    }

    /// Resamples `input`, the next chunk, appending the result to `output`. The output trails the
    /// input by two samples, which the next chunk brings out.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        for &sample in input {
            if self.primed < 3 {
                // The first sample stands as its own history.
                if self.primed == 0 {
                    self.window = [sample; 4];
                } else {
                    self.window[self.primed + 1] = sample;
                }
                self.primed += 1;
                if self.primed < 3 {
                    continue;
                }
                self.window[0] = self.window[1];
            } else {
                self.window = [self.window[1], self.window[2], self.window[3], sample];
            }
            while self.fraction < 1.0 {
                output.push(hermite(self.window, self.fraction));
                self.fraction += self.step;
            }
            self.fraction -= 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn pair(config: StreamConfig) -> (StreamReader, VoiceStream) {
        StreamReader::pair(config, RATE, VoiceId(1))
    }

    fn sine(frequency: f32, rate: u32, frames: usize, phase: usize) -> Vec<f32> {
        #[allow(clippy::cast_precision_loss)]
        (phase..phase + frames)
            .map(|n| (std::f32::consts::TAU * frequency * n as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn the_ring_keeps_order_wraps_and_refuses_what_wont_fit() {
        let ring = Ring::new(8);
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 6);
        assert_eq!(ring.pop(), Some(1.0));
        assert_eq!(ring.pop(), Some(2.0));
        assert_eq!(ring.push(&[7.0, 8.0, 9.0, 10.0, 11.0]), 4);
        assert_eq!(ring.len(), 8);
        ring.discard(3);
        let rest: Vec<f32> = std::iter::from_fn(|| ring.pop()).collect();
        assert_eq!(rest, [6.0, 7.0, 8.0, 9.0, 10.0]);
    }

    #[test]
    fn the_ring_carries_samples_across_threads_whole() {
        let ring = Arc::new(Ring::new(64));
        let producer = Arc::clone(&ring);
        let thread = std::thread::spawn(move || {
            let mut next = 0_u16;
            while next < 20_000 {
                let chunk: Vec<f32> = (next..next.saturating_add(7).min(20_000))
                    .map(f32::from)
                    .collect();
                let taken = producer.push(&chunk);
                next += u16::try_from(taken).unwrap();
            }
            producer.close();
        });
        let mut expected = 0.0;
        while !(ring.is_closed() && ring.len() == 0) {
            if let Some(sample) = ring.pop() {
                assert!((sample - expected).abs() < f32::EPSILON);
                expected += 1.0;
            }
        }
        thread.join().unwrap();
        assert!((expected - 20_000.0).abs() < f32::EPSILON);
    }

    /// A stream waits for its latency before it plays, fades in, plays what it was given at the
    /// same rate unchanged, and ends after its writer is gone.
    #[test]
    fn a_stream_holds_its_latency_then_plays_what_it_was_given() {
        let (mut reader, mut writer) = pair(StreamConfig::new(RATE));
        let tone = sine(440.0, RATE, 4_800, 0);
        writer.push(&tone[..1_440]);
        let mut heard = Vec::new();
        for _ in 0..1_000 {
            heard.push(reader.next().unwrap());
        }
        assert!(
            heard.iter().all(|&sample| sample == 0.0),
            "played before 60 ms were in"
        );
        writer.push(&tone[1_440..2_880]);
        heard.clear();
        for _ in 0..2_400 {
            heard.push(reader.next().unwrap());
        }
        // Past the fade in, it is the tone itself, from its start, at most a hair's lean away.
        for (n, sample) in heard.iter().enumerate().skip(200).take(1_000) {
            assert!(
                (sample - tone[n]).abs() < 0.02,
                "{n}: {sample} for {}",
                tone[n]
            );
        }
        drop(writer);
        let mut frames = 0;
        while reader.next().is_some() {
            frames += 1;
            assert!(frames < 10_000, "never ended");
        }
    }

    /// A stream pushed faster than it plays, as from a sound card whose clock runs fast, settles
    /// at its latency by playing faster, and never falls behind into a catch-up.
    #[test]
    fn a_fast_clock_is_taken_up_by_leaning_the_rate() {
        let (mut reader, mut writer) = pair(StreamConfig::new(RATE));
        // 0.2% fast: 962 samples for every 960 played, in 20 ms packets.
        let mut sent = 0;
        let mut fills = Vec::new();
        for packet in 0..1_500 {
            writer.push(&sine(300.0, RATE, 962, sent));
            sent += 962;
            for _ in 0..960 {
                reader.next().unwrap();
                assert!(!reader.catching_up, "fell behind at packet {packet}");
            }
            fills.push(reader.ring.len());
        }
        let settled = fills[1_000..].iter().sum::<usize>() / 500;
        assert!(
            (2_000..5_000).contains(&settled),
            "the buffer drifted to {settled} samples rather than near 2 880"
        );
    }

    /// A stream that's fallen far behind drops what's late rather than staying behind, and a dry
    /// one fades to silence without a click.
    #[test]
    fn a_stream_far_behind_catches_up_and_a_dry_one_fades() {
        let (mut reader, mut writer) = pair(StreamConfig::new(RATE));
        writer.push(&vec![0.5; 2_880]);
        for _ in 0..1_000 {
            reader.next();
        }
        writer.push(&vec![0.5; 14_400]);
        for _ in 0..1_000 {
            reader.next();
        }
        assert!(
            reader.ring.len() < 4_000,
            "still {} behind",
            reader.ring.len()
        );

        let mut previous = reader.next().unwrap();
        for _ in 0..10_000 {
            let sample = reader.next().unwrap();
            assert!(
                (sample - previous).abs() < 0.01,
                "a click from {previous} to {sample}"
            );
            previous = sample;
        }
        assert!(previous.abs() < f32::EPSILON, "ran dry but still sounding");
    }

    /// A phrase shorter than the latency plays once the writer has gone quiet a latency's time.
    #[test]
    fn a_short_phrase_isnt_held_back() {
        let (mut reader, mut writer) = pair(StreamConfig::new(RATE));
        writer.push(&vec![0.5; 480]);
        let heard: Vec<f32> = (0..6_000).map(|_| reader.next().unwrap()).collect();
        assert!(heard.iter().any(|&sample| sample > 0.4));
    }

    #[test]
    fn the_writer_knows_when_its_voice_is_gone() {
        let (reader, mut writer) = pair(StreamConfig::new(RATE));
        assert!(writer.is_playing());
        drop(reader);
        assert!(!writer.is_playing());
        writer.push(&[0.0; 16]);
    }

    /// Chunked resampling joins without a seam, matches a tone at the new rate, and comes out at
    /// the ratio of the rates.
    #[test]
    fn resampling_in_chunks_is_seamless() {
        let input = sine(1_000.0, 44_100, 44_100, 0);
        let mut resampler = StreamResampler::new(44_100, RATE);
        let mut output = Vec::new();
        for chunk in input.chunks(441) {
            resampler.process(chunk, &mut output);
        }
        assert!(output.len().abs_diff(48_000) < 4, "{}", output.len());
        let expected = sine(1_000.0, RATE, output.len(), 0);
        let worst = output
            .iter()
            .zip(&expected)
            .skip(8)
            .fold(0.0_f32, |worst, (a, b)| worst.max((a - b).abs()));
        assert!(worst < 0.005, "{worst}");
    }
}
