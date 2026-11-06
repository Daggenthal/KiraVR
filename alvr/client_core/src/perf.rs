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

    #[test]
    fn old_server_stats_are_dropped() {
        let mut tracker = PerfTracker::default();
        tracker.on_server_stats(PerfStats::default());
        assert!(tracker.snapshot().server.is_some());
        tracker.server.as_mut().unwrap().1 -= SERVER_STATS_MAX_AGE + Duration::from_millis(1);
        assert!(tracker.snapshot().server.is_none());
    }
}
