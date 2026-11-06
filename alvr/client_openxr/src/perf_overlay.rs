// KiraVR: In-VR performance overlay. The input thread watches for the
// controller shortcut, the render thread cycles Hidden, Compact, Full and
// refreshes the text twice a second from the headset's and the streamer's
// numbers.

use alvr_client_core::{ClientCoreContext, OverlayPoses, perf::PerfSnapshot};
use alvr_common::{
    Pose, ViewParams,
    glam::{Mat3, Mat4, Quat, UVec2, Vec3},
    inputs::{
        LEFT_SQUEEZE_CLICK_ID, LEFT_SQUEEZE_VALUE_ID, LEFT_THUMBSTICK_CLICK_ID, LEFT_X_CLICK_ID,
        RIGHT_A_CLICK_ID, RIGHT_SQUEEZE_CLICK_ID, RIGHT_SQUEEZE_VALUE_ID,
        RIGHT_THUMBSTICK_CLICK_ID,
    },
};
use alvr_graphics::{OverlayAnchor, OverlayLine, OverlayRenderer};
use alvr_packets::{ButtonEntry, ButtonValue};
use alvr_session::{
    CodecType, PerformanceOverlayConfig, PerformanceOverlayMode, PerformanceOverlayPosition,
    PerformanceOverlayShortcut,
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const TEXT_REFRESH: Duration = Duration::from_millis(500);

// Panel width in meters at 1 m in view (scaled with distance so it keeps its
// apparent size) and on the wrist.
const IN_VIEW_WIDTH_PER_METER: f32 = 0.30;
const WRIST_WIDTH: f32 = 0.11;
// From the controller grip toward the elbow, and out of the back of the hand.
const WRIST_TOWARD_ELBOW: f32 = 0.12;
const WRIST_OUT_OF_HAND: f32 = 0.045;
// Facing (cosine of the angle between panel and eyes) where the wrist panel
// starts to show and where it is fully visible.
const WRIST_FADE_START: f32 = 0.45;
const WRIST_FADE_END: f32 = 0.75;
// Grabbing: How close a controller must be to the panel's center, how hard
// the grip must be squeezed, and two grabs this close together reset the
// panel to its default spot.
const GRAB_RADIUS: f32 = 0.10;
const GRIP_PRESSED: f32 = 0.6;
const DOUBLE_GRAB: Duration = Duration::from_millis(400);
// Where a controller points, in grip space (measured on Quest 3, see
// default_local_pose).
const GRIP_FORWARD: Vec3 = Vec3::NEG_Y;

// A vibration for one controller (0 left, 1 right), for feedback when the
// panel may be out of sight, like on the wrist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HapticPulse {
    pub hand: usize,
    pub duration: Duration,
    pub amplitude: f32,
}

const fn pulse(hand: usize, millis: u64, amplitude: f32) -> HapticPulse {
    HapticPulse {
        hand,
        duration: Duration::from_millis(millis),
        amplitude,
    }
}

const GREEN: [u8; 3] = [110, 230, 140];
const YELLOW: [u8; 3] = [250, 210, 90];
const RED: [u8; 3] = [255, 110, 100];
const WHITE: [u8; 3] = [235, 235, 240];
const GRAY: [u8; 3] = [165, 170, 180];

// Overlay settings shared by the input and render threads, None when turned
// off. Updated live when they change in the dashboard.
pub type SharedOverlayConfig =
    Arc<alvr_common::parking_lot::Mutex<Option<PerformanceOverlayConfig>>>;

fn shortcut_buttons(shortcut: PerformanceOverlayShortcut) -> Vec<u64> {
    match shortcut {
        PerformanceOverlayShortcut::BothThumbsticks => {
            vec![*LEFT_THUMBSTICK_CLICK_ID, *RIGHT_THUMBSTICK_CLICK_ID]
        }
        PerformanceOverlayShortcut::LeftThumbstickAndX => {
            vec![*LEFT_THUMBSTICK_CLICK_ID, *LEFT_X_CLICK_ID]
        }
        PerformanceOverlayShortcut::RightThumbstickAndA => {
            vec![*RIGHT_THUMBSTICK_CLICK_ID, *RIGHT_A_CLICK_ID]
        }
    }
}

// Watches button changes from the input thread and counts completed holds.
pub struct ShortcutDetector {
    config: SharedOverlayConfig,
    pressed: HashSet<u64>,
    held_since: Option<Instant>,
    fired: bool,
    triggers: Arc<AtomicUsize>,
    // Grip state per hand, for grabbing the panel.
    grips_down: Arc<[AtomicBool; 2]>,
}

impl ShortcutDetector {
    pub fn new(
        config: SharedOverlayConfig,
        triggers: Arc<AtomicUsize>,
        grips_down: Arc<[AtomicBool; 2]>,
    ) -> Self {
        Self {
            config,
            pressed: HashSet::new(),
            held_since: None,
            fired: false,
            triggers,
            grips_down,
        }
    }

    // Call every input poll, also when nothing changed, so a hold completes.
    pub fn update(&mut self, changes: &[ButtonEntry]) {
        self.update_at(Instant::now(), changes);
    }

    fn update_at(&mut self, now: Instant, changes: &[ButtonEntry]) {
        for entry in changes {
            // Controllers report the grip as a value (Quest) or a click.
            let hand = if entry.path_id == *LEFT_SQUEEZE_VALUE_ID
                || entry.path_id == *LEFT_SQUEEZE_CLICK_ID
            {
                Some(0)
            } else if entry.path_id == *RIGHT_SQUEEZE_VALUE_ID
                || entry.path_id == *RIGHT_SQUEEZE_CLICK_ID
            {
                Some(1)
            } else {
                None
            };
            if let Some(hand) = hand {
                let down = match entry.value {
                    ButtonValue::Binary(down) => down,
                    ButtonValue::Scalar(value) => value > GRIP_PRESSED,
                };
                self.grips_down[hand].store(down, Ordering::Relaxed);
            }
        }

        // Read live, the shortcut and hold time can change while streaming.
        let Some((buttons, hold)) = self.config.lock().as_ref().map(|config| {
            (
                shortcut_buttons(config.shortcut),
                Duration::from_secs_f32(config.hold_seconds.max(0.1)),
            )
        }) else {
            self.pressed.clear();
            self.held_since = None;
            return;
        };

        for entry in changes {
            if !buttons.contains(&entry.path_id) {
                continue;
            }
            let down = match entry.value {
                ButtonValue::Binary(down) => down,
                ButtonValue::Scalar(value) => value > 0.5,
            };
            if down {
                self.pressed.insert(entry.path_id);
            } else {
                self.pressed.remove(&entry.path_id);
            }
        }

        self.pressed.retain(|b| buttons.contains(b));
        let all_down = buttons.iter().all(|b| self.pressed.contains(b));
        if !all_down {
            self.held_since = None;
            self.fired = false;
            return;
        }
        let since = *self.held_since.get_or_insert(now);
        if !self.fired && now.saturating_duration_since(since) >= hold {
            // Once per hold: release the buttons to trigger again.
            self.fired = true;
            self.triggers.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// What the overlay shows besides live numbers, fixed for the session.
#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub view_resolution: UVec2,
    pub refresh_hz: f32,
    pub codec: Option<CodecType>,
    pub wired: bool,
}

// A panel held by a controller: Its pose in that hand's space.
struct Grab {
    hand: usize,
    hand_to_panel: Mat4,
}

pub struct PerfOverlay {
    enabled: bool,
    position: PerformanceOverlayPosition,
    size: f32,
    grab_range: f32,
    mode: PerformanceOverlayMode,
    triggers: Arc<AtomicUsize>,
    seen_triggers: usize,
    last_text: Option<Instant>,
    grips_down: Arc<[AtomicBool; 2]>,
    was_down: [bool; 2],
    grab: Option<Grab>,
    last_grab_start: Option<Instant>,
    poses: OverlayPoses,
    haptics: Vec<HapticPulse>,
}

impl PerfOverlay {
    pub fn new(
        config: Option<&PerformanceOverlayConfig>,
        triggers: Arc<AtomicUsize>,
        grips_down: Arc<[AtomicBool; 2]>,
        overlay: &OverlayRenderer,
        poses: OverlayPoses,
    ) -> Self {
        let mode = config.map_or(PerformanceOverlayMode::Hidden, |c| c.initial_mode);
        let mut this = Self {
            enabled: false,
            position: PerformanceOverlayPosition::LeftWrist,
            size: 1.0,
            grab_range: 1.0,
            mode,
            seen_triggers: triggers.load(Ordering::Relaxed),
            triggers,
            last_text: None,
            grips_down,
            was_down: [false; 2],
            grab: None,
            last_grab_start: None,
            poses,
            haptics: vec![],
        };
        this.apply_config(overlay, config);
        this
    }

    // Takes over settings changed in the dashboard. The current mode stays.
    pub fn apply_config(
        &mut self,
        overlay: &OverlayRenderer,
        config: Option<&PerformanceOverlayConfig>,
    ) {
        match config {
            Some(config) => {
                if std::mem::discriminant(&config.position)
                    != std::mem::discriminant(&self.position)
                {
                    self.grab = None;
                }
                self.enabled = true;
                self.position = config.position;
                self.size = config.size;
                self.grab_range = config.grab_range;
                overlay.set_visible(self.mode != PerformanceOverlayMode::Hidden);
            }
            None => {
                self.enabled = false;
                self.grab = None;
                overlay.set_visible(false);
            }
        }
        self.last_text = None;
    }

    // Vibrations queued since the last call, for the caller to play.
    pub fn take_haptics(&mut self) -> Vec<HapticPulse> {
        std::mem::take(&mut self.haptics)
    }

    fn custom_pose(&mut self) -> &mut Option<Pose> {
        match self.position {
            PerformanceOverlayPosition::LeftWrist => &mut self.poses.left_wrist,
            PerformanceOverlayPosition::RightWrist => &mut self.poses.right_wrist,
            PerformanceOverlayPosition::InView { .. } => &mut self.poses.in_view,
        }
    }

    // Once per rendered frame. `views` and `grips` (left, right controller)
    // are in the same space, for the time the frame is displayed.
    pub fn frame(
        &mut self,
        overlay: &OverlayRenderer,
        core: &ClientCoreContext,
        info: &StreamInfo,
        views: &[ViewParams; 2],
        grips: [Option<Pose>; 2],
    ) {
        if !self.enabled {
            return;
        }
        self.place(overlay, core, views, grips);

        let triggers = self.triggers.load(Ordering::Relaxed);
        if triggers != self.seen_triggers {
            for _ in self.seen_triggers..triggers {
                self.mode = next_mode(self.mode);
            }
            // Short for compact, long for full, soft for hidden, on both
            // controllers since the shortcut may use either.
            let (millis, amplitude) = match self.mode {
                PerformanceOverlayMode::Compact => (60, 0.6),
                PerformanceOverlayMode::Full => (160, 0.6),
                PerformanceOverlayMode::Hidden => (40, 0.3),
            };
            self.haptics
                .extend([pulse(0, millis, amplitude), pulse(1, millis, amplitude)]);
            self.seen_triggers = triggers;
            overlay.set_visible(self.mode != PerformanceOverlayMode::Hidden);
            self.last_text = None;
        }

        if self.mode == PerformanceOverlayMode::Hidden {
            return;
        }
        if self.last_text.is_some_and(|t| t.elapsed() < TEXT_REFRESH) {
            return;
        }
        self.last_text = Some(Instant::now());
        overlay.update(&build_lines(self.mode, &core.perf_snapshot(), info));
    }

    // Positions the panel, and lets a controller grab and move it.
    fn place(
        &mut self,
        overlay: &OverlayRenderer,
        core: &ClientCoreContext,
        views: &[ViewParams; 2],
        grips: [Option<Pose>; 2],
    ) {
        let down = [
            self.grips_down[0].load(Ordering::Relaxed),
            self.grips_down[1].load(Ordering::Relaxed),
        ];
        let pressed_now = [down[0] && !self.was_down[0], down[1] && !self.was_down[1]];
        self.was_down = down;

        let head = head_pose(views);
        let (parent, grabbing_hands) = match self.position {
            PerformanceOverlayPosition::InView { .. } => (Some(head), [true, true]),
            // Moved with the other hand.
            PerformanceOverlayPosition::LeftWrist => (grips[0], [false, true]),
            PerformanceOverlayPosition::RightWrist => (grips[1], [true, false]),
        };
        // The panel keeps its size while held.
        let custom = *self.custom_pose();
        let (_, width, _) = anchor_for(self.position, self.size, views, grips, custom);

        // Held: Follow the hand, store the result when let go.
        if let Some(grab) = &self.grab {
            let hand = grab.hand;
            if let Some(hand_pose) = grips[hand] {
                let panel = pose_mat(hand_pose) * grab.hand_to_panel;
                if down[hand] {
                    overlay.set_anchor(
                        OverlayAnchor::World {
                            pose: mat_pose(panel),
                        },
                        width,
                        1.0,
                    );
                    return;
                }
                if let Some(parent) = parent {
                    *self.custom_pose() = Some(mat_pose(pose_mat(parent).inverse() * panel));
                    core.store_overlay_poses(self.poses);
                    self.haptics.push(pulse(hand, 30, 0.4));
                }
            }
            self.grab = None;
        }

        // Recomputed, a release above may have just moved it.
        let custom = *self.custom_pose();
        let (anchor, width, opacity) = anchor_for(self.position, self.size, views, grips, custom);
        overlay.set_anchor(anchor, width, opacity);

        // Grab: A grip squeezed close to the visible panel.
        if self.mode == PerformanceOverlayMode::Hidden || opacity < 0.5 {
            return;
        }
        let panel = match anchor {
            OverlayAnchor::Head { pose } => pose_mat(head) * pose_mat(pose),
            OverlayAnchor::World { pose } => pose_mat(pose),
        };
        let center = panel.w_axis.truncate();
        for hand in 0..2 {
            let (true, true, Some(hand_pose)) =
                (grabbing_hands[hand], pressed_now[hand], grips[hand])
            else {
                continue;
            };
            // Up close, or pointed at from within the grab range.
            let pointing = hand_pose.orientation * GRIP_FORWARD;
            if hand_pose.position.distance(center) > GRAB_RADIUS
                && !ray_hits_panel(hand_pose.position, pointing, panel, width, self.grab_range)
            {
                continue;
            }
            // Two quick grabs put it back where it started.
            if self
                .last_grab_start
                .is_some_and(|t| t.elapsed() < DOUBLE_GRAB)
            {
                *self.custom_pose() = None;
                core.store_overlay_poses(self.poses);
                self.last_grab_start = None;
                self.haptics.push(pulse(hand, 200, 0.7));
                return;
            }
            self.last_grab_start = Some(Instant::now());
            self.grab = Some(Grab {
                hand,
                hand_to_panel: pose_mat(hand_pose).inverse() * panel,
            });
            self.haptics.push(pulse(hand, 30, 0.4));
            return;
        }
    }
}

// Whether a ray (a controller pointing) hits the panel within `range` meters.
// The panel's readable side faces its local +Z, centered on `panel`'s origin.
// The hit area is a bit larger than the text so it is easy to aim at.
pub fn ray_hits_panel(origin: Vec3, direction: Vec3, panel: Mat4, width: f32, range: f32) -> bool {
    let to_local = panel.inverse();
    let o = to_local.transform_point3(origin);
    let d = to_local.transform_vector3(direction.normalize_or_zero());
    if d.z.abs() < 1e-4 {
        return false;
    }
    // Where the ray crosses the panel's plane (z = 0).
    let t = -o.z / d.z;
    if t < 0.0 || t > range {
        return false;
    }
    let hit = o + d * t;
    let half = width * 0.6;
    hit.x.abs() <= half && hit.y.abs() <= half
}

fn pose_mat(pose: Pose) -> Mat4 {
    Mat4::from_rotation_translation(pose.orientation, pose.position)
}

fn mat_pose(mat: Mat4) -> Pose {
    let (_, orientation, position) = mat.to_scale_rotation_translation();
    Pose {
        orientation,
        position,
    }
}

// Between the eyes, facing where they face.
fn head_pose(views: &[ViewParams; 2]) -> Pose {
    Pose {
        orientation: views[0].pose.orientation,
        position: (views[0].pose.position + views[1].pose.position) / 2.0,
    }
}

// The built-in panel pose relative to its parent (head or controller grip).
fn default_local_pose(position: PerformanceOverlayPosition) -> Pose {
    match position {
        PerformanceOverlayPosition::InView {
            horizontal,
            vertical,
            distance,
        } => Pose {
            orientation: Quat::IDENTITY,
            position: Vec3::new(horizontal, vertical, -distance.max(0.1)),
        },
        // Grip axes as measured on Quest 3 (2026-11-06, controller held like
        // reading a watch, then pointed forward): -X out of the back of the
        // left hand, -Y forward toward the fingers (+Y toward the elbow), -Z
        // up. This is not what the OpenXR spec text suggests for X, so it was
        // measured rather than taken from the spec. The panel's columns are
        // its right, up and facing directions in grip space: It faces out of
        // the back of the wrist, with the text's up toward the fingers.
        PerformanceOverlayPosition::LeftWrist => Pose {
            orientation: Quat::from_mat3(&Mat3::from_cols(-Vec3::Z, -Vec3::Y, -Vec3::X)),
            position: Vec3::new(-WRIST_OUT_OF_HAND, WRIST_TOWARD_ELBOW, 0.0),
        },
        // Mirrored for the right hand (back of the hand is +X). Not measured.
        PerformanceOverlayPosition::RightWrist => Pose {
            orientation: Quat::from_mat3(&Mat3::from_cols(Vec3::Z, -Vec3::Y, Vec3::X)),
            position: Vec3::new(WRIST_OUT_OF_HAND, WRIST_TOWARD_ELBOW, 0.0),
        },
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// Where the panel goes this frame, its width in meters and its opacity.
// `custom` replaces the built-in pose relative to the head or wrist.
pub fn anchor_for(
    position: PerformanceOverlayPosition,
    size: f32,
    views: &[ViewParams; 2],
    grips: [Option<Pose>; 2],
    custom: Option<Pose>,
) -> (OverlayAnchor, f32, f32) {
    let size = size.max(0.1);
    let local = custom.unwrap_or_else(|| default_local_pose(position));

    let grip = match position {
        PerformanceOverlayPosition::InView { .. } => {
            // Keeps its apparent size at any distance.
            let distance = local.position.length().max(0.1);
            return (
                OverlayAnchor::Head { pose: local },
                IN_VIEW_WIDTH_PER_METER * size * distance,
                1.0,
            );
        }
        PerformanceOverlayPosition::LeftWrist => grips[0],
        PerformanceOverlayPosition::RightWrist => grips[1],
    };

    let width = WRIST_WIDTH * size;
    let Some(grip) = grip else {
        // Controller not tracked: Hide instead of floating somewhere.
        return (
            OverlayAnchor::World {
                pose: Pose::IDENTITY,
            },
            width,
            0.0,
        );
    };

    let pose = mat_pose(pose_mat(grip) * pose_mat(local));
    let facing = pose.orientation * Vec3::Z;
    let to_head = (head_pose(views).position - pose.position).normalize_or_zero();
    let opacity = smoothstep(WRIST_FADE_START, WRIST_FADE_END, facing.dot(to_head));

    (OverlayAnchor::World { pose }, width, opacity)
}

pub fn next_mode(mode: PerformanceOverlayMode) -> PerformanceOverlayMode {
    match mode {
        PerformanceOverlayMode::Hidden => PerformanceOverlayMode::Compact,
        PerformanceOverlayMode::Compact => PerformanceOverlayMode::Full,
        PerformanceOverlayMode::Full => PerformanceOverlayMode::Hidden,
    }
}

fn fps_color(fps: f32, refresh_hz: f32) -> [u8; 3] {
    let ratio = if refresh_hz > 0.0 {
        fps / refresh_hz
    } else {
        1.0
    };
    if ratio >= 0.95 {
        GREEN
    } else if ratio >= 0.8 {
        YELLOW
    } else {
        RED
    }
}

fn level(value: f32, good_below: f32, bad_from: f32) -> [u8; 3] {
    if value < good_below {
        GREEN
    } else if value < bad_from {
        YELLOW
    } else {
        RED
    }
}

pub fn build_lines(
    mode: PerformanceOverlayMode,
    snap: &PerfSnapshot,
    info: &StreamInfo,
) -> Vec<OverlayLine> {
    if mode == PerformanceOverlayMode::Hidden {
        return vec![];
    }
    let client = &snap.client;
    let mut lines = vec![];

    let Some(server) = &snap.server else {
        lines.push(OverlayLine::new(
            format!("{:.0} fps received", client.received_fps),
            fps_color(client.received_fps, info.refresh_hz),
        ));
        lines.push(OverlayLine::new("Waiting for streamer stats...", GRAY));
        return lines;
    };

    // Compact: The four numbers that say whether things are fine.
    lines.push(OverlayLine::new(
        format!("{:.0} fps", server.client_fps),
        fps_color(server.client_fps, info.refresh_hz),
    ));
    lines.push(OverlayLine::new(
        format!("Latency {:.0} ms", server.total_ms),
        level(server.total_ms, 60.0, 90.0),
    ));
    lines.push(OverlayLine::new(
        format!("{:.1} Mbps", server.bitrate_mbps),
        WHITE,
    ));
    lines.push(OverlayLine::new(
        format!(
            "Loss {:.1}%  Jitter {:.1} ms",
            client.loss_percent, client.jitter_ms
        ),
        if client.loss_percent >= 1.0 || client.jitter_ms >= 8.0 {
            RED
        } else if client.loss_percent > 0.0 || client.jitter_ms >= 3.0 {
            YELLOW
        } else {
            GREEN
        },
    ));

    if mode == PerformanceOverlayMode::Full {
        let codec = match info.codec {
            Some(CodecType::H264) => "H.264",
            Some(CodecType::Hevc) => "HEVC",
            Some(CodecType::AV1) => "AV1",
            None => "?",
        };
        lines.push(OverlayLine::new(
            format!(
                "{}x{} @ {:.0} Hz  {codec}  {}",
                info.view_resolution.x,
                info.view_resolution.y,
                info.refresh_hz,
                if info.wired { "Wired" } else { "Wi-Fi" }
            ),
            GRAY,
        ));
        lines.push(OverlayLine::new(
            format!(
                "Streamer {:.0} fps  Headset {:.0} fps",
                server.server_fps, server.client_fps
            ),
            fps_color(server.server_fps.min(server.client_fps), info.refresh_hz),
        ));
        lines.push(OverlayLine::new("Latency Breakdown", WHITE));
        let stage = |name: &str, ms: f32| format!("  {name} {ms:.1}");
        lines.push(OverlayLine::new(
            [
                stage("Game", server.game_ms),
                stage("Compose", server.server_compositor_ms),
                stage("Encode", server.encoder_ms),
            ]
            .join(""),
            GRAY,
        ));
        lines.push(OverlayLine::new(
            [
                stage("Network", server.network_ms),
                stage("Decode", server.decoder_ms),
            ]
            .join(""),
            level(server.network_ms, 10.0, 25.0),
        ));
        lines.push(OverlayLine::new(
            [
                stage("Queue", server.decoder_queue_ms),
                stage("Render", server.client_compositor_ms),
                stage("Vsync", server.vsync_queue_ms),
            ]
            .join(""),
            GRAY,
        ));
        lines.push(OverlayLine::new(
            format!("Packets lost this session: {}", client.lost_total),
            if client.lost_total == 0 { GREEN } else { GRAY },
        ));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_client_core::perf::ClientPerf;
    use alvr_packets::PerfStats;

    fn config(shortcut: PerformanceOverlayShortcut) -> PerformanceOverlayConfig {
        PerformanceOverlayConfig {
            shortcut,
            hold_seconds: 1.0,
            position: PerformanceOverlayPosition::LeftWrist,
            size: 1.0,
            grab_range: 1.0,
            initial_mode: PerformanceOverlayMode::Hidden,
        }
    }

    fn press(id: u64, down: bool) -> ButtonEntry {
        ButtonEntry {
            path_id: id,
            value: ButtonValue::Binary(down),
        }
    }

    #[test]
    fn shortcut_fires_once_per_hold() {
        let triggers = Arc::new(AtomicUsize::new(0));
        let grips = Arc::new([AtomicBool::new(false), AtomicBool::new(false)]);
        let shared = Arc::new(alvr_common::parking_lot::Mutex::new(Some(config(
            PerformanceOverlayShortcut::BothThumbsticks,
        ))));
        let mut detector = ShortcutDetector::new(
            Arc::clone(&shared),
            Arc::clone(&triggers),
            Arc::clone(&grips),
        );
        let t0 = Instant::now();
        detector.update_at(t0, &[press(*LEFT_THUMBSTICK_CLICK_ID, true)]);
        detector.update_at(t0 + Duration::from_secs(2), &[]);
        assert_eq!(
            triggers.load(Ordering::Relaxed),
            0,
            "one stick is not the shortcut"
        );

        detector.update_at(
            t0 + Duration::from_secs(2),
            &[press(*RIGHT_THUMBSTICK_CLICK_ID, true)],
        );
        detector.update_at(t0 + Duration::from_millis(2500), &[]);
        assert_eq!(triggers.load(Ordering::Relaxed), 0, "not held long enough");

        detector.update_at(t0 + Duration::from_millis(3100), &[]);
        detector.update_at(t0 + Duration::from_secs(5), &[]);
        assert_eq!(triggers.load(Ordering::Relaxed), 1, "fires once while held");

        detector.update_at(
            t0 + Duration::from_secs(6),
            &[press(*LEFT_THUMBSTICK_CLICK_ID, false)],
        );
        detector.update_at(
            t0 + Duration::from_secs(6),
            &[press(*LEFT_THUMBSTICK_CLICK_ID, true)],
        );
        detector.update_at(t0 + Duration::from_secs(8), &[]);
        assert_eq!(
            triggers.load(Ordering::Relaxed),
            2,
            "fires again after release"
        );
    }

    fn eyes_at(head: Vec3) -> [ViewParams; 2] {
        let eye = |x: f32| ViewParams {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: head + Vec3::new(x, 0.0, 0.0),
            },
            ..ViewParams::DUMMY
        };
        [eye(-0.032), eye(0.032)]
    }

    #[test]
    fn in_view_keeps_its_apparent_size() {
        let views = eyes_at(Vec3::new(0.0, 1.6, 0.0));
        let at = |distance| PerformanceOverlayPosition::InView {
            horizontal: 0.0,
            vertical: 0.0,
            distance,
        };
        let (_, near_width, near_opacity) = anchor_for(at(1.0), 1.0, &views, [None; 2], None);
        let (_, far_width, _) = anchor_for(at(2.0), 1.0, &views, [None; 2], None);
        assert_eq!(near_opacity, 1.0);
        assert!((far_width - 2.0 * near_width).abs() < 1e-5);
    }

    #[test]
    fn wrist_shows_only_when_turned_toward_the_eyes() {
        let head = Vec3::new(0.0, 1.6, 0.0);
        let views = eyes_at(head);
        let center = Vec3::new(0.0, 1.2, -0.35);
        // Grip -X (back of the left hand, measured) is the panel's facing
        // direction.
        let grip_facing = |direction: Vec3| Pose {
            orientation: Quat::from_rotation_arc(-Vec3::X, direction.normalize()),
            position: center,
        };
        let toward = grip_facing(head - center);
        let away = grip_facing(center - head);

        let (anchor, width, opacity) = anchor_for(
            PerformanceOverlayPosition::LeftWrist,
            1.0,
            &views,
            [Some(toward), None],
            None,
        );
        assert!(opacity > 0.95, "facing the eyes: {opacity}");
        assert!((width - WRIST_WIDTH).abs() < 1e-6);
        let OverlayAnchor::World { pose } = anchor else {
            panic!("wrist anchor must be in world space")
        };
        // Pushed back toward the elbow, off the controller.
        assert!(pose.position.distance(center) > 0.1);

        let (_, _, opacity) = anchor_for(
            PerformanceOverlayPosition::LeftWrist,
            1.0,
            &views,
            [Some(away), None],
            None,
        );
        assert_eq!(opacity, 0.0, "turned away");

        let (_, _, opacity) = anchor_for(
            PerformanceOverlayPosition::LeftWrist,
            1.0,
            &views,
            [None; 2],
            None,
        );
        assert_eq!(opacity, 0.0, "untracked controller");
    }

    #[test]
    fn pointing_grabs_the_panel_within_range() {
        // Panel 2 m ahead of the origin, facing back toward it.
        let panel = Mat4::from_translation(Vec3::new(0.0, 0.0, -2.0));
        let forward = -Vec3::Z;
        assert!(ray_hits_panel(Vec3::ZERO, forward, panel, 0.3, 3.0));
        assert!(
            !ray_hits_panel(Vec3::ZERO, forward, panel, 0.3, 1.5),
            "out of range"
        );
        assert!(
            !ray_hits_panel(Vec3::ZERO, Vec3::Z, panel, 0.3, 3.0),
            "pointing away"
        );
        // Aiming 0.5 m to the side of a 0.3 m panel misses.
        let aside = (Vec3::new(0.5, 0.0, -2.0)).normalize();
        assert!(!ray_hits_panel(Vec3::ZERO, aside, panel, 0.3, 3.0));
        // Slightly off center still counts.
        let near_edge = (Vec3::new(0.12, 0.05, -2.0)).normalize();
        assert!(ray_hits_panel(Vec3::ZERO, near_edge, panel, 0.3, 3.0));
    }

    #[test]
    fn modes_cycle() {
        use PerformanceOverlayMode::*;
        assert_eq!(next_mode(Hidden), Compact);
        assert_eq!(next_mode(Compact), Full);
        assert_eq!(next_mode(Full), Hidden);
    }

    #[test]
    fn compact_and_full_lines() {
        let info = StreamInfo {
            view_resolution: UVec2::new(2144, 1072),
            refresh_hz: 72.0,
            codec: Some(CodecType::Hevc),
            wired: false,
        };
        let snap = PerfSnapshot {
            client: ClientPerf {
                received_fps: 72.0,
                loss_percent: 0.0,
                jitter_ms: 0.7,
                lost_total: 4,
            },
            server: Some(PerfStats {
                server_fps: 72.0,
                client_fps: 72.0,
                total_ms: 55.2,
                network_ms: 3.3,
                bitrate_mbps: 31.1,
                ..Default::default()
            }),
        };
        let compact = build_lines(PerformanceOverlayMode::Compact, &snap, &info);
        assert_eq!(compact.len(), 4);
        assert_eq!(compact[0].text, "72 fps");
        assert_eq!(compact[0].color, GREEN);
        assert_eq!(compact[1].text, "Latency 55 ms");
        assert_eq!(compact[3].color, GREEN);

        let full = build_lines(PerformanceOverlayMode::Full, &snap, &info);
        assert!(full.len() > compact.len());
        assert!(
            full.iter()
                .any(|l| l.text.contains("2144x1072 @ 72 Hz  HEVC  Wi-Fi"))
        );

        assert!(build_lines(PerformanceOverlayMode::Hidden, &snap, &info).is_empty());
    }

    #[test]
    fn bad_numbers_turn_red() {
        let info = StreamInfo {
            view_resolution: UVec2::new(2144, 1072),
            refresh_hz: 90.0,
            codec: None,
            wired: true,
        };
        let snap = PerfSnapshot {
            client: ClientPerf {
                received_fps: 50.0,
                loss_percent: 3.0,
                jitter_ms: 12.0,
                lost_total: 99,
            },
            server: Some(PerfStats {
                client_fps: 50.0,
                total_ms: 120.0,
                ..Default::default()
            }),
        };
        let lines = build_lines(PerformanceOverlayMode::Compact, &snap, &info);
        assert_eq!(lines[0].color, RED);
        assert_eq!(lines[1].color, RED);
        assert_eq!(lines[3].color, RED);
    }
}
