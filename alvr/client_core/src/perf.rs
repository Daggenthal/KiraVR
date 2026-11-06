// KiraVR: Data for the in-VR performance overlay. The headset measures what
// only it can see (frames arriving, packet loss, jitter), the streamer sends
// its side as PerfStats every 0.5 s.

use alvr_packets::PerfStats;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(1);
// Streamer numbers older than this are not shown, they belong to a stall.
const SERVER_STATS_MAX_AGE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientPerf {
    // Video frames that arrived per second.
    pub received_fps: f32,
    // Share of video packets lost in the last window, 0 to 100.
    pub loss_percent: f32,
    // Spread (standard deviation) of the time between arriving frames.
    pub jitter_ms: f32,
    pub lost_total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PerfSnapshot {
    pub client: ClientPerf,
    pub server: Option<PerfStats>,
    pub battery: Option<BatteryEstimate>,
}

// One battery reading, taken every BATTERY_SAMPLE_INTERVAL.
#[derive(Clone, Copy, Debug)]
pub struct BatterySample {
    pub at: Instant,
    // 0 to 1
    pub level: f32,
    pub plugged: bool,
    pub voltage_mv: Option<i32>,
    pub charge_counter_uah: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PowerFlow {
    // Not enough history yet for a steady number.
    Estimating,
    Charging,
    Discharging,
    // Plugged in, but the charger gives less than the headset uses.
    PluggedButDraining,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryEstimate {
    // 0 to 1
    pub level: f32,
    pub plugged: bool,
    pub flow: PowerFlow,
    // Power into (charging) or out of (draining) the battery, always >= 0.
    pub watts: Option<f32>,
    // Until 100% when charging, until 10% when draining.
    pub time_left: Option<Duration>,
}

pub const BATTERY_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
// Estimates come from this much history, so they do not jump around.
const BATTERY_WINDOW: Duration = Duration::from_secs(5 * 60);
// Shortest history that gives a usable number (charge counters move in steps).
const BATTERY_MIN_SPAN: Duration = Duration::from_secs(90);
const DRAIN_TARGET: f32 = 0.10;

// Estimates battery power and time from a rolling window of readings. Reading
// the battery is cheap, the window is what keeps the numbers steady.
#[derive(Default)]
pub struct BatteryTracker {
    samples: Vec<BatterySample>,
}

impl BatteryTracker {
    pub fn add(&mut self, sample: BatterySample) {
        // Plugging in or out changes the trend completely, start over.
        if self
            .samples
            .last()
            .is_some_and(|last| last.plugged != sample.plugged)
        {
            self.samples.clear();
        }
        self.samples.push(sample);
        let oldest = sample.at.checked_sub(BATTERY_WINDOW);
        if let Some(oldest) = oldest {
            self.samples.retain(|s| s.at >= oldest);
        }
    }

    pub fn estimate(&self) -> Option<BatteryEstimate> {
        let last = *self.samples.last()?;
        let first = self.samples[0];
        let span = last.at.saturating_duration_since(first.at);

        let mut estimate = BatteryEstimate {
            level: last.level,
            plugged: last.plugged,
            flow: PowerFlow::Estimating,
            watts: None,
            time_left: None,
        };
        if span < BATTERY_MIN_SPAN {
            return Some(estimate);
        }
        let hours = span.as_secs_f32() / 3600.0;

        // Preferred: The charge counter, in microampere-hours. Fall back to
        // the percentage, which is coarse but always there.
        let (rate_per_hour, capacity) = match (first.charge_counter_uah, last.charge_counter_uah) {
            (Some(a), Some(b)) if last.level > 0.02 => {
                let capacity = b as f32 / last.level;
                ((b - a) as f32 / hours, Some(capacity))
            }
            _ => ((last.level - first.level) / hours, None),
        };

        // A flat trend over the window: Not moving either way yet.
        let level_rate = match capacity {
            Some(capacity) => rate_per_hour / capacity,
            None => rate_per_hour,
        };
        if level_rate.abs() < 1e-4 {
            return Some(estimate);
        }

        estimate.flow = match (level_rate > 0.0, last.plugged) {
            (true, _) => PowerFlow::Charging,
            (false, true) => PowerFlow::PluggedButDraining,
            (false, false) => PowerFlow::Discharging,
        };

        if let (Some(_), Some(voltage)) = (capacity, last.voltage_mv) {
            // microamperes * volts = microwatts
            estimate.watts = Some((rate_per_hour.abs() / 1e6) * (voltage as f32 / 1000.0));
        }

        let target = if level_rate > 0.0 { 1.0 } else { DRAIN_TARGET };
        let to_go = (target - last.level) / level_rate;
        if to_go >= 0.0 && to_go.is_finite() {
            estimate.time_left = Some(Duration::from_secs_f32(to_go * 3600.0));
        } else if level_rate < 0.0 {
            // Already below the target.
            estimate.time_left = Some(Duration::ZERO);
        }
        Some(estimate)
    }
}

pub struct PerfTracker {
    window_start: Instant,
    last_arrival: Option<Instant>,
    intervals_ms: Vec<f32>,
    received: u32,
    lost: u32,
    lost_total: u64,
    client: ClientPerf,
    server: Option<(PerfStats, Instant)>,
    battery: BatteryTracker,
}

impl Default for PerfTracker {
    fn default() -> Self {
        Self {
            window_start: Instant::now(),
            last_arrival: None,
            intervals_ms: Vec::with_capacity(128),
            received: 0,
            lost: 0,
            lost_total: 0,
            client: ClientPerf::default(),
            server: None,
            battery: BatteryTracker::default(),
        }
    }
}

impl PerfTracker {
    pub fn on_video_packet(&mut self, lost_before: u32) {
        self.on_video_packet_at(Instant::now(), lost_before);
    }

    fn on_video_packet_at(&mut self, now: Instant, lost_before: u32) {
        if let Some(last) = self.last_arrival {
            self.intervals_ms
                .push(now.saturating_duration_since(last).as_secs_f32() * 1000.0);
        }
        self.last_arrival = Some(now);
        self.received += 1;
        self.lost += lost_before;
        self.lost_total += lost_before as u64;

        let elapsed = now.saturating_duration_since(self.window_start);
        if elapsed >= WINDOW {
            self.close_window(elapsed);
            self.window_start = now;
        }
    }

    fn close_window(&mut self, elapsed: Duration) {
        let total = self.received + self.lost;
        self.client = ClientPerf {
            received_fps: self.received as f32 / elapsed.as_secs_f32(),
            loss_percent: if total == 0 {
                0.0
            } else {
                self.lost as f32 * 100.0 / total as f32
            },
            jitter_ms: std_dev(&self.intervals_ms),
            lost_total: self.lost_total,
        };
        self.intervals_ms.clear();
        self.received = 0;
        self.lost = 0;
    }

    // New stream, same headset: Keeps the battery history.
    pub fn reset_stream_stats(&mut self) {
        let battery = std::mem::take(&mut self.battery);
        *self = Self {
            battery,
            ..Self::default()
        };
    }

    pub fn on_battery_sample(&mut self, sample: BatterySample) {
        self.battery.add(sample);
    }

    pub fn on_server_stats(&mut self, stats: PerfStats) {
        self.server = Some((stats, Instant::now()));
    }

    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            client: self.client.clone(),
            server: self
                .server
                .as_ref()
                .filter(|(_, at)| at.elapsed() < SERVER_STATS_MAX_AGE)
                .map(|(stats, _)| stats.clone()),
            battery: self.battery.estimate(),
        }
    }
}

fn std_dev(values: &[f32]) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let variance =
        values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / (values.len() - 1) as f32;
    variance.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tracker: &mut PerfTracker, start: Instant, intervals_ms: &[u64], lost: &[u32]) {
        let mut t = start;
        for (i, ms) in intervals_ms.iter().enumerate() {
            t += Duration::from_millis(*ms);
            tracker.on_video_packet_at(t, lost.get(i).copied().unwrap_or(0));
        }
    }

    #[test]
    fn steady_stream_has_no_jitter_or_loss() {
        let start = Instant::now();
        let mut tracker = PerfTracker {
            window_start: start,
            ..Default::default()
        };
        // 72 fps for a bit over a second
        feed(&mut tracker, start, &[14; 75], &[]);
        let client = tracker.snapshot().client;
        assert!((client.received_fps - 71.4).abs() < 2.0, "{client:?}");
        assert!(client.jitter_ms < 0.01, "{client:?}");
        assert_eq!(client.loss_percent, 0.0);
    }

    #[test]
    fn counts_loss_and_jitter() {
        let start = Instant::now();
        let mut tracker = PerfTracker {
            window_start: start,
            ..Default::default()
        };
        // 50 frames, alternating 10 and 30 ms apart, 10 packets lost on the way
        let intervals = (0..50)
            .map(|i| if i % 2 == 0 { 10 } else { 30 })
            .collect::<Vec<_>>();
        let mut lost = vec![0; 50];
        lost[20] = 10;
        feed(&mut tracker, start, &intervals, &lost);
        let client = tracker.snapshot().client;
        // 10 lost of 60 sent
        assert!((client.loss_percent - 16.7).abs() < 0.5, "{client:?}");
        assert!((client.jitter_ms - 10.0).abs() < 0.5, "{client:?}");
        assert_eq!(client.lost_total, 10);
    }

    fn sample(at: Instant, level: f32, counter: i64, plugged: bool) -> BatterySample {
        BatterySample {
            at,
            level,
            plugged,
            voltage_mv: Some(3800),
            charge_counter_uah: Some(counter),
        }
    }

    // A 5000 mAh battery losing 1500 mA: 1.5 A * 3.8 V = 5.7 W.
    #[test]
    fn discharging_power_and_time_to_ten_percent() {
        let mut tracker = BatteryTracker::default();
        let t0 = Instant::now();
        assert_eq!(tracker.estimate(), None);
        for i in 0..=10u64 {
            // 30 s steps: 1.5 A for 30 s is 12500 uAh
            let counter = 2_500_000 - 12_500 * i as i64;
            tracker.add(sample(
                t0 + BATTERY_SAMPLE_INTERVAL * i as u32,
                counter as f32 / 5e6,
                counter,
                false,
            ));
        }
        let estimate = tracker.estimate().unwrap();
        assert_eq!(estimate.flow, PowerFlow::Discharging);
        let watts = estimate.watts.unwrap();
        assert!((watts - 5.7).abs() < 0.1, "{watts}");
        // From about 47.5% to 10% of 5000 mAh at 1.5 A: about 1.25 h.
        let hours = estimate.time_left.unwrap().as_secs_f32() / 3600.0;
        assert!((hours - 1.25).abs() < 0.05, "{hours}");
    }

    #[test]
    fn plugged_in_but_still_draining_is_called_out() {
        let mut tracker = BatteryTracker::default();
        let t0 = Instant::now();
        for i in 0..=6u64 {
            let counter = 800_000 - 2_000 * i as i64;
            tracker.add(sample(
                t0 + BATTERY_SAMPLE_INTERVAL * i as u32,
                counter as f32 / 5e6,
                counter,
                true,
            ));
        }
        assert_eq!(
            tracker.estimate().unwrap().flow,
            PowerFlow::PluggedButDraining
        );
    }

    #[test]
    fn charging_counts_up_to_full() {
        let mut tracker = BatteryTracker::default();
        let t0 = Instant::now();
        for i in 0..=6u64 {
            let counter = 4_000_000 + 10_000 * i as i64;
            tracker.add(sample(
                t0 + BATTERY_SAMPLE_INTERVAL * i as u32,
                counter as f32 / 5e6,
                counter,
                true,
            ));
        }
        let estimate = tracker.estimate().unwrap();
        assert_eq!(estimate.flow, PowerFlow::Charging);
        assert!(estimate.time_left.unwrap() > Duration::ZERO);
    }

    #[test]
    fn estimating_until_enough_history_and_after_plug_change() {
        let mut tracker = BatteryTracker::default();
        let t0 = Instant::now();
        tracker.add(sample(t0, 0.5, 2_500_000, false));
        tracker.add(sample(t0 + BATTERY_SAMPLE_INTERVAL, 0.5, 2_490_000, false));
        assert_eq!(tracker.estimate().unwrap().flow, PowerFlow::Estimating);
        // Plugging in starts the window over.
        tracker.add(sample(
            t0 + BATTERY_SAMPLE_INTERVAL * 6,
            0.5,
            2_400_000,
            true,
        ));
        assert_eq!(tracker.estimate().unwrap().flow, PowerFlow::Estimating);
    }

    #[test]
    fn falls_back_to_the_percentage_without_a_charge_counter() {
        let mut tracker = BatteryTracker::default();
        let t0 = Instant::now();
        for i in 0..=10u64 {
            tracker.add(BatterySample {
                at: t0 + BATTERY_SAMPLE_INTERVAL * i as u32,
                level: 0.60 - 0.002 * i as f32,
                plugged: false,
                voltage_mv: Some(3800),
                charge_counter_uah: None,
            });
        }
        let estimate = tracker.estimate().unwrap();
        assert_eq!(estimate.flow, PowerFlow::Discharging);
        assert_eq!(estimate.watts, None, "no watts without a charge counter");
        assert!(estimate.time_left.is_some());
    }

    #[test]
    fn old_server_stats_are_dropped() {
        let mut tracker = PerfTracker::default();
        tracker.on_server_stats(PerfStats::default());
        assert!(tracker.snapshot().server.is_some());
        tracker.server.as_mut().unwrap().1 -= SERVER_STATS_MAX_AGE + Duration::from_millis(1);
        assert!(tracker.snapshot().server.is_none());
    }
}
