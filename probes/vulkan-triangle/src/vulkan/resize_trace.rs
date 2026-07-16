use std::env;
use std::time::{Duration, Instant};

#[derive(Default)]
struct TimingSeries {
    samples: u32,
    total: Duration,
    maximum: Duration,
}

impl TimingSeries {
    fn record(&mut self, duration: Duration) {
        self.samples += 1;
        self.total += duration;
        self.maximum = self.maximum.max(duration);
    }

    fn average_ms(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1_000.0 / f64::from(self.samples)
        }
    }

    fn maximum_ms(&self) -> f64 {
        self.maximum.as_secs_f64() * 1_000.0
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct LiveResizeSample {
    pub(super) frame_wait: Duration,
    pub(super) recreate: Option<Duration>,
    pub(super) acquire: Duration,
    pub(super) record_submit: Duration,
    pub(super) present: Duration,
}

pub(super) struct LiveResizeTrace {
    enabled: bool,
    reported: bool,
    attempts: u64,
    rendered: u64,
    recreations: u64,
    last_attempt: Option<Instant>,
    callback_interval: TimingSeries,
    frame_total: TimingSeries,
    frame_wait: TimingSeries,
    recreate: TimingSeries,
    acquire: TimingSeries,
    record_submit: TimingSeries,
    present: TimingSeries,
}

impl LiveResizeTrace {
    pub(super) fn from_environment() -> Self {
        Self {
            enabled: env::var_os("MULCIBER_VULKAN_RESIZE_TRACE").is_some(),
            reported: false,
            attempts: 0,
            rendered: 0,
            recreations: 0,
            last_attempt: None,
            callback_interval: TimingSeries::default(),
            frame_total: TimingSeries::default(),
            frame_wait: TimingSeries::default(),
            recreate: TimingSeries::default(),
            acquire: TimingSeries::default(),
            record_submit: TimingSeries::default(),
            present: TimingSeries::default(),
        }
    }

    pub(super) fn begin(&mut self, live_resize: bool) -> Option<Instant> {
        if !self.enabled {
            return None;
        }
        if !live_resize {
            self.last_attempt = None;
            return None;
        }
        let now = Instant::now();
        if let Some(previous) = self.last_attempt.replace(now) {
            self.callback_interval.record(now.duration_since(previous));
        }
        self.attempts += 1;
        Some(now)
    }

    pub(super) const fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn finish(
        &mut self,
        started: Option<Instant>,
        sample: LiveResizeSample,
        rendered: bool,
    ) {
        let Some(started) = started else {
            return;
        };
        if rendered {
            self.rendered += 1;
        }
        self.frame_total.record(started.elapsed());
        self.frame_wait.record(sample.frame_wait);
        if let Some(recreate) = sample.recreate {
            self.recreations += 1;
            self.recreate.record(recreate);
        }
        self.acquire.record(sample.acquire);
        self.record_submit.record(sample.record_submit);
        self.present.record(sample.present);
    }

    pub(super) fn report(&mut self) {
        if !self.enabled || self.reported {
            return;
        }
        self.reported = true;
        println!(
            "Live resize trace: attempts={} rendered={} recreations={}",
            self.attempts, self.rendered, self.recreations
        );
        for (name, series) in [
            ("callback interval", &self.callback_interval),
            ("frame total", &self.frame_total),
            ("frame-fence wait", &self.frame_wait),
            ("swapchain recreation", &self.recreate),
            ("image acquisition", &self.acquire),
            ("record + submit", &self.record_submit),
            ("queue present", &self.present),
        ] {
            println!(
                "  {name}: samples={} avg={:.3} ms max={:.3} ms",
                series.samples,
                series.average_ms(),
                series.maximum_ms()
            );
        }
    }
}
