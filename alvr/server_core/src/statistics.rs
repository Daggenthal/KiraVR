use alvr_common::{HEAD_ID, SlidingWindowAverage};
use alvr_events::{BitrateDirectives, EventType, GraphStatistics, StatisticsSummary};
use alvr_packets::{ClientStatistics, PerfStats};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const FULL_REPORT_INTERVAL: Duration = Duration::from_millis(500);
const EPS_INTERVAL: Duration = Duration::from_micros(1);

pub struct HistoryFrame {
    target_timestamp: Duration,
    tracking_received: Instant,
    frame_present: Instant,
    frame_composed: Instant,
    frame_encoded: Instant,
    video_packet_bytes: usize,
    total_pipeline_latency: Duration,
}

impl Default for HistoryFrame {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            target_timestamp: Duration::ZERO,
            tracking_received: now,
            frame_present: now,
            frame_composed: now,
            frame_encoded: now,
            video_packet_bytes: 0,
            total_pipeline_latency: Duration::ZERO,
        }
    }
}

#[derive(Default, Clone)]
struct BatteryData {
    gauge_value: f32,
    is_plugged: bool,
}

// KiraVR: Sums over one summary window, averaged into a PerfStats snapshot for
// the headset's performance overlay. Single frames are too noisy to read.
// KiraVR: Frames the driver dropped before encoding (on Linux the encode
// mailbox keeps only the newest frame). Counted from the driver thread,
// turned into a rate per second by the statistics.
static STREAMER_DROPS: AtomicU64 = AtomicU64::new(0);

pub fn report_streamer_drop() {
    STREAMER_DROPS.fetch_add(1, Ordering::Relaxed);
}

#[derive(Default)]
struct PerfWindow {
    frames: u32,
    sums_s: [f32; 9],
    client_fps: f32,
    server_fps: f32,
    streamer_dropped_fps: f32,
}

impl PerfWindow {
    // Order: total, game, server compositor, encoder, network, decoder,
    // decoder queue, client compositor, vsync queue.
    fn add(
        &mut self,
        latencies: [Duration; 9],
        client_fps: f32,
        server_fps: f32,
        streamer_dropped_fps: f32,
    ) {
        self.frames += 1;
        self.streamer_dropped_fps += streamer_dropped_fps;
        for (sum, latency) in self.sums_s.iter_mut().zip(latencies) {
            *sum += latency.as_secs_f32();
        }
        self.client_fps += client_fps;
        self.server_fps += server_fps;
    }

    fn take(&mut self, bitrate_mbps: f32) -> Option<PerfStats> {
        if self.frames == 0 {
            return None;
        }
        let n = self.frames as f32;
        let ms = |i: usize| self.sums_s[i] / n * 1000.0;
        let stats = PerfStats {
            server_fps: self.server_fps / n,
            client_fps: self.client_fps / n,
            total_ms: ms(0),
            game_ms: ms(1),
            server_compositor_ms: ms(2),
            encoder_ms: ms(3),
            network_ms: ms(4),
            decoder_ms: ms(5),
            decoder_queue_ms: ms(6),
            client_compositor_ms: ms(7),
            vsync_queue_ms: ms(8),
            bitrate_mbps,
            streamer_dropped_fps: self.streamer_dropped_fps / n,
        };
        *self = Self::default();
        Some(stats)
    }
}

pub struct StatisticsManager {
    perf_window: PerfWindow,
    perf_snapshot: Option<PerfStats>,
    history_buffer: VecDeque<HistoryFrame>,
    max_history_size: usize,
    last_full_report_instant: Instant,
    last_frame_present_instant: Instant,
    last_frame_present_interval: Duration,
    video_packets_total: usize,
    video_packets_partial_sum: usize,
    video_bytes_total: usize,
    video_bytes_partial_sum: usize,
    battery_gauges: HashMap<u64, BatteryData>,
    steamvr_pipeline_latency: Duration,
    motion_to_photon_latency_average: SlidingWindowAverage<Duration>,
    last_vsync_time: Instant,
    frame_interval: Duration,
    last_throughput_directives: BitrateDirectives,
    // (when, total drops so far), the last second of them.
    drop_samples: VecDeque<(Instant, u64)>,
}

impl StatisticsManager {
    // history size used to calculate average total pipeline latency
    pub fn new(
        max_history_size: usize,
        nominal_server_frame_interval: Duration,
        steamvr_pipeline_frames: f32,
    ) -> Self {
        Self {
            perf_window: PerfWindow::default(),
            perf_snapshot: None,
            history_buffer: VecDeque::new(),
            max_history_size,
            last_full_report_instant: Instant::now(),
            last_frame_present_instant: Instant::now(),
            last_frame_present_interval: Duration::ZERO,
            video_packets_total: 0,
            video_packets_partial_sum: 0,
            video_bytes_total: 0,
            video_bytes_partial_sum: 0,
            battery_gauges: HashMap::new(),
            steamvr_pipeline_latency: Duration::from_secs_f32(
                steamvr_pipeline_frames * nominal_server_frame_interval.as_secs_f32(),
            ),
            motion_to_photon_latency_average: SlidingWindowAverage::new(
                Duration::ZERO,
                max_history_size,
            ),
            last_vsync_time: Instant::now(),
            frame_interval: nominal_server_frame_interval,
            last_throughput_directives: BitrateDirectives::default(),
            drop_samples: VecDeque::new(),
        }
    }

    pub fn report_tracking_received(&mut self, target_timestamp: Duration) {
        if !self
            .history_buffer
            .iter()
            .any(|frame| frame.target_timestamp == target_timestamp)
        {
            self.history_buffer.push_front(HistoryFrame {
                target_timestamp,
                tracking_received: Instant::now(),
                ..Default::default()
            });
        }

        if self.history_buffer.len() > self.max_history_size {
            self.history_buffer.pop_back();
        }
    }

    pub fn report_frame_present(&mut self, target_timestamp: Duration, offset: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            let now = Instant::now() - offset;

            self.last_frame_present_interval =
                now.saturating_duration_since(self.last_frame_present_instant);
            self.last_frame_present_instant = now;

            frame.frame_present = now;
        }
    }

    pub fn report_frame_composed(&mut self, target_timestamp: Duration, offset: Duration) {
        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            frame.frame_composed = Instant::now() - offset;
        }
    }

    // returns encoding interval
    pub fn report_frame_encoded(
        &mut self,
        target_timestamp: Duration,
        bytes_count: usize,
    ) -> Duration {
        self.video_packets_total += 1;
        self.video_packets_partial_sum += 1;
        self.video_bytes_total += bytes_count;
        self.video_bytes_partial_sum += bytes_count;

        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == target_timestamp)
        {
            frame.frame_encoded = Instant::now();

            frame.video_packet_bytes = bytes_count;

            frame
                .frame_encoded
                .saturating_duration_since(frame.frame_composed)
        } else {
            Duration::ZERO
        }
    }

    pub fn report_battery(&mut self, device_id: u64, gauge_value: f32, is_plugged: bool) {
        *self.battery_gauges.entry(device_id).or_default() = BatteryData {
            gauge_value,
            is_plugged,
        };
    }

    pub fn report_throughput_stats(&mut self, stats: BitrateDirectives) {
        self.last_throughput_directives = stats;
    }

    // KiraVR: Driver drops per second over the last second.
    fn streamer_dropped_fps(&mut self) -> f32 {
        let now = Instant::now();
        self.drop_samples
            .push_back((now, STREAMER_DROPS.load(Ordering::Relaxed)));
        while self.drop_samples.len() > 2
            && now.duration_since(self.drop_samples[1].0) >= Duration::from_secs(1)
        {
            self.drop_samples.pop_front();
        }
        let (first_at, first) = self.drop_samples[0];
        let span = now.duration_since(first_at).as_secs_f32();
        if span < 0.2 {
            return 0.0;
        }
        let last = self.drop_samples.back().unwrap().1;
        last.saturating_sub(first) as f32 / span
    }

    // Called every frame. Some statistics are reported once every frame
    // Returns (network latency, game time latency)
    pub fn report_statistics(&mut self, client_stats: ClientStatistics) -> (Duration, Duration) {
        self.motion_to_photon_latency_average
            .submit_sample(client_stats.total_pipeline_latency);
        // Before borrowing the history below.
        let streamer_dropped_fps = self.streamer_dropped_fps();

        if let Some(frame) = self
            .history_buffer
            .iter_mut()
            .find(|frame| frame.target_timestamp == client_stats.target_timestamp)
        {
            frame.total_pipeline_latency = client_stats.total_pipeline_latency;

            let game_time_latency = frame
                .frame_present
                .saturating_duration_since(frame.tracking_received);

            let server_compositor_latency = frame
                .frame_composed
                .saturating_duration_since(frame.frame_present);

            let encoder_latency = frame
                .frame_encoded
                .saturating_duration_since(frame.frame_composed);

            // The network latency cannot be estiamed directly. It is what's left of the total
            // latency after subtracting all other latency intervals. In particular it contains the
            // transport latency of the tracking packet and the interval between the first video
            // packet is sent and the last video packet is received for a specific frame.
            // For safety, use saturating_sub to avoid a crash if for some reason the network
            // latency is miscalculated as negative.
            let network_latency = frame.total_pipeline_latency.saturating_sub(
                game_time_latency
                    + server_compositor_latency
                    + encoder_latency
                    + client_stats.video_decode
                    + client_stats.video_decoder_queue
                    + client_stats.rendering
                    + client_stats.vsync_queue,
            );

            let client_fps =
                1.0 / Duration::max(client_stats.frame_interval, EPS_INTERVAL).as_secs_f32();
            let server_fps =
                1.0 / Duration::max(self.last_frame_present_interval, EPS_INTERVAL).as_secs_f32();

            self.perf_window.add(
                [
                    client_stats.total_pipeline_latency,
                    game_time_latency,
                    server_compositor_latency,
                    encoder_latency,
                    network_latency,
                    client_stats.video_decode,
                    client_stats.video_decoder_queue,
                    client_stats.rendering,
                    client_stats.vsync_queue,
                ],
                client_fps,
                server_fps,
                streamer_dropped_fps,
            );

            if self.last_full_report_instant + FULL_REPORT_INTERVAL < Instant::now() {
                self.last_full_report_instant += FULL_REPORT_INTERVAL;

                let interval_secs = FULL_REPORT_INTERVAL.as_secs_f32();

                alvr_events::send_event(EventType::StatisticsSummary(StatisticsSummary {
                    video_packets_total: self.video_packets_total,
                    video_packets_per_sec: (self.video_packets_partial_sum as f32 / interval_secs)
                        as _,
                    video_mbytes_total: (self.video_bytes_total as f32 / 1e6) as usize,
                    video_mbits_per_sec: self.video_bytes_partial_sum as f32 * 8.
                        / 1e6
                        / interval_secs,
                    total_latency_ms: client_stats.total_pipeline_latency.as_secs_f32() * 1000.,
                    network_latency_ms: network_latency.as_secs_f32() * 1000.,
                    encode_latency_ms: encoder_latency.as_secs_f32() * 1000.,
                    decode_latency_ms: client_stats.video_decode.as_secs_f32() * 1000.,
                    client_fps: client_fps as _,
                    server_fps: server_fps as _,
                    battery_hmd: (self
                        .battery_gauges
                        .get(&HEAD_ID)
                        .cloned()
                        .unwrap_or_default()
                        .gauge_value
                        * 100.) as u32,
                    hmd_plugged: self
                        .battery_gauges
                        .get(&HEAD_ID)
                        .cloned()
                        .unwrap_or_default()
                        .is_plugged,
                }));

                self.perf_snapshot = self
                    .perf_window
                    .take(self.video_bytes_partial_sum as f32 * 8. / 1e6 / interval_secs);

                self.video_packets_partial_sum = 0;
                self.video_bytes_partial_sum = 0;
            }

            let packet_bits = frame.video_packet_bytes as f32 * 8.0;
            let throughput_bps =
                packet_bits / Duration::max(network_latency, EPS_INTERVAL).as_secs_f32();
            let bitrate_bps = packet_bits
                / Duration::max(self.last_frame_present_interval, EPS_INTERVAL).as_secs_f32();

            // todo: use target timestamp in nanoseconds. the dashboard needs to use the first
            // timestamp as the graph time origin.
            alvr_events::send_event(EventType::GraphStatistics(GraphStatistics {
                total_pipeline_latency_s: client_stats.total_pipeline_latency.as_secs_f32(),
                game_time_s: game_time_latency.as_secs_f32(),
                server_compositor_s: server_compositor_latency.as_secs_f32(),
                encoder_s: encoder_latency.as_secs_f32(),
                network_s: network_latency.as_secs_f32(),
                decoder_s: client_stats.video_decode.as_secs_f32(),
                decoder_queue_s: client_stats.video_decoder_queue.as_secs_f32(),
                client_compositor_s: client_stats.rendering.as_secs_f32(),
                vsync_queue_s: client_stats.vsync_queue.as_secs_f32(),
                client_fps,
                server_fps,
                bitrate_directives: self.last_throughput_directives.clone(),
                throughput_bps,
                bitrate_bps,
                streamer_dropped_fps,
            }));

            (network_latency, game_time_latency)
        } else {
            (Duration::ZERO, Duration::ZERO)
        }
    }

    // The latest overlay snapshot, once per summary window.
    pub fn take_perf_snapshot(&mut self) -> Option<PerfStats> {
        self.perf_snapshot.take()
    }

    pub fn motion_to_photon_latency_average(&self) -> Duration {
        self.motion_to_photon_latency_average.get_average()
    }

    pub fn tracker_pose_time_offset(&self) -> Duration {
        // This is the opposite of the client's StatisticsManager::tracker_prediction_offset().
        self.steamvr_pipeline_latency
    }

    // NB: this call is non-blocking, waiting should be done externally
    pub fn duration_until_next_vsync(&mut self) -> Duration {
        let now = Instant::now();

        // update the last vsync if it's too old
        while self.last_vsync_time + self.frame_interval < now {
            self.last_vsync_time += self.frame_interval;
        }

        (self.last_vsync_time + self.frame_interval).saturating_duration_since(now)
    }
}
