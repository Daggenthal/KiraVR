use alvr_common::{
    AlvrFoveatedEncodingParams, BodySkeleton, ConnectionState, DeviceMotion, LogSeverity, Pose,
    ViewParams,
    anyhow::{Error, Result},
    glam::{Quat, UVec2, Vec2},
    semver::Version,
};
use alvr_session::{
    ClientsidePostProcessingConfig, CodecType, PassthroughMode, PerformanceLevel, SessionConfig,
    Settings,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json as json;
use std::{
    collections::HashSet,
    fmt::{self, Debug, Write as _},
    net::IpAddr,
    sync::LazyLock,
    time::Duration,
};

pub const TRACKING: u16 = 0;
pub const HAPTICS: u16 = 1;
pub const AUDIO: u16 = 2;
pub const VIDEO: u16 = 3;
pub const STATISTICS: u16 = 4;

#[derive(Serialize, Deserialize, Clone)]
pub struct VideoStreamingCapabilitiesExt {
    // Nothing for now
}

#[derive(Serialize, Deserialize, Clone)]
pub struct VideoStreamingCapabilities {
    pub default_view_resolution: UVec2,
    pub max_view_resolution: UVec2,
    pub refresh_rates: Vec<f32>,
    pub microphone_sample_rate: u32,
    pub foveated_encoding: bool,
    pub encoder_high_profile: bool,
    pub encoder_10_bits: bool,
    pub encoder_av1: bool,
    pub prefer_10bit: bool,
    pub preferred_encoding_gamma: f32,
    pub prefer_hdr: bool,
    pub ext_str: String,
}

impl VideoStreamingCapabilities {
    pub fn with_ext(self, ext: VideoStreamingCapabilitiesExt) -> Self {
        Self {
            ext_str: json::to_string(&ext).unwrap(),
            ..self
        }
    }

    pub fn ext(&self) -> Result<VideoStreamingCapabilitiesExt> {
        let _ext_json = json::from_str::<json::Value>(&self.ext_str)?;

        // decode values here

        Ok(VideoStreamingCapabilitiesExt {})
    }
}

#[derive(Serialize, Deserialize)]
pub struct ConnectionAcceptedInfo {
    pub client_protocol_id: u64,
    pub platform_string: String,
    pub server_ip: IpAddr, // must be unused for now
    pub streaming_capabilities: Option<VideoStreamingCapabilities>,
}

#[derive(Serialize, Deserialize)]
pub enum ClientConnectionResult {
    ConnectionAccepted(Box<ConnectionAcceptedInfo>),
    ClientStandby,
}

#[derive(Serialize, Deserialize)]
pub struct NegotiatedStreamingConfigExt {
    // Nothing for now
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClientNegotiatedStreamingConfig {
    pub view_resolution: UVec2,
    pub refresh_rate_hint: f32,
    pub game_audio_sample_rate: u32,
    pub foveated_encoding: Option<AlvrFoveatedEncodingParams>,
    pub encoding_gamma: f32,
    pub enable_hdr: bool,
    pub wired: bool,
    pub ext_str: String,
}

impl ClientNegotiatedStreamingConfig {
    pub fn with_ext(self, ext: NegotiatedStreamingConfigExt) -> Self {
        Self {
            ext_str: json::to_string(&ext).unwrap(),
            ..self
        }
    }

    pub fn ext(&self) -> Result<NegotiatedStreamingConfigExt> {
        let _ext_json = json::from_str::<json::Value>(&self.ext_str)?;

        // decode values here

        Ok(NegotiatedStreamingConfigExt {})
    }
}

#[derive(Serialize, Deserialize)]
pub struct StreamConfigPacket {
    pub session: String, // JSON session that allows for extrapolation
    pub negotiated: ClientNegotiatedStreamingConfig,
}

// KiraVR: Version-independent views of the two handshake packets. Each holds
// only the first field, which has the same type in every version, and bincode
// ignores the bytes after it. That lets each side read just enough of a peer
// on another version to explain the mismatch instead of failing silently.
// Field order of ConnectionAcceptedInfo, ClientConnectionResult and
// StreamConfigPacket must keep these first fields first.

#[derive(Serialize, Deserialize)]
pub struct ConnectionAcceptedHead {
    pub client_protocol_id: u64,
}

#[derive(Serialize, Deserialize)]
pub enum ClientConnectionResultHead {
    ConnectionAccepted(ConnectionAcceptedHead),
    ClientStandby,
}

// Sent instead of StreamConfigPacket when the client is incompatible. Same
// leading field, so any client decoding a StreamConfigHead can read it.
#[derive(Serialize, Deserialize)]
pub struct StreamConfigHead {
    pub session: String,
}

pub const INCOMPATIBLE_NOTICE_KEY: &str = "kiravr_incompatible";

// KiraVR: Live streamer-side numbers for the performance overlay, averaged
// over the statistics window (0.5 s). Sent as ServerControlPacket::Reserved,
// which ALVR clients ignore, so this needs no protocol change.
pub const PERF_STATS_KEY: &str = "kiravr_perf";

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PerfStats {
    pub server_fps: f32,
    pub client_fps: f32,
    pub total_ms: f32,
    pub game_ms: f32,
    pub server_compositor_ms: f32,
    pub encoder_ms: f32,
    pub network_ms: f32,
    pub decoder_ms: f32,
    pub decoder_queue_ms: f32,
    pub client_compositor_ms: f32,
    pub vsync_queue_ms: f32,
    pub bitrate_mbps: f32,
    // Frames the streamer dropped before encoding, per second. Default for
    // streamers that do not send it.
    #[serde(default)]
    pub streamer_dropped_fps: f32,
}

// KiraVR: The overlay settings, sent whenever they change in the dashboard so
// the headset applies them without reconnecting. None means turned off.
pub const OVERLAY_CONFIG_KEY: &str = "kiravr_overlay_config";

pub fn overlay_config_to_reserved(
    config: &Option<alvr_session::PerformanceOverlayConfig>,
) -> String {
    json::json!({ OVERLAY_CONFIG_KEY: config }).to_string()
}

// Outer None: Not an overlay config packet.
pub fn overlay_config_from_reserved(
    text: &str,
) -> Option<Option<alvr_session::PerformanceOverlayConfig>> {
    let value = json::from_str::<json::Value>(text).ok()?;
    json::from_value(value.get(OVERLAY_CONFIG_KEY)?.clone()).ok()
}

impl PerfStats {
    pub fn to_reserved(&self) -> String {
        json::json!({ PERF_STATS_KEY: self }).to_string()
    }

    // None for Reserved packets that carry something else.
    pub fn from_reserved(text: &str) -> Option<Self> {
        let value = json::from_str::<json::Value>(text).ok()?;
        json::from_value(value.get(PERF_STATS_KEY)?.clone()).ok()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IncompatibleNotice {
    pub server_version: String,
    pub server_protocol: String,
    // Written by the server, which knows both versions. Shown on the headset.
    pub message: String,
}

impl IncompatibleNotice {
    pub fn to_head(&self) -> Result<StreamConfigHead> {
        Ok(StreamConfigHead {
            session: json::to_string(&json::json!({ INCOMPATIBLE_NOTICE_KEY: self }))?,
        })
    }

    // None when the session JSON is a normal session.
    pub fn from_head(head: &StreamConfigHead) -> Option<Self> {
        let value = json::from_str::<json::Value>(&head.session).ok()?;
        json::from_value(value.get(INCOMPATIBLE_NOTICE_KEY)?.clone()).ok()
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClientStreamConfig {
    pub server_version: Version,
    pub settings: Settings,
    pub negotiated_config: ClientNegotiatedStreamingConfig,
}

impl StreamConfigPacket {
    pub fn new(
        session: &SessionConfig,
        negotiated: ClientNegotiatedStreamingConfig,
    ) -> Result<Self> {
        Ok(Self {
            session: json::to_string(session)?,
            negotiated,
        })
    }

    pub fn to_stream_config(self) -> Result<ClientStreamConfig> {
        let mut session_config = SessionConfig::default();
        session_config.merge_from_json(&json::from_str(&self.session)?)?;
        let settings = session_config.to_settings();

        Ok(ClientStreamConfig {
            server_version: session_config.server_version,
            settings,
            negotiated_config: self.negotiated,
        })
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DecoderInitializationConfig {
    pub codec: CodecType,
    pub config_buffer: Vec<u8>, // e.g. SPS + PPS NALs
    pub ext_str: String,
}

#[derive(Serialize, Deserialize)]
pub enum ServerControlPacket {
    StartStream,
    DecoderConfig(DecoderInitializationConfig),
    Restarting,
    KeepAlive,
    RealTimeConfig(RealTimeConfig),
    Reserved(String),
    ReservedBuffer(Vec<u8>),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BatteryInfo {
    pub device_id: u64,
    pub gauge_value: f32, // range [0, 1]
    pub is_plugged: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum ButtonValue {
    Binary(bool),
    Scalar(f32),
}

#[derive(Serialize, Deserialize)]
pub struct ButtonEntry {
    pub path_id: u64,
    pub value: ButtonValue,
}

#[derive(Serialize, Deserialize)]
pub enum ClientControlPacket {
    PlayspaceSync(Option<Vec2>),
    RequestIdr,
    KeepAlive,
    StreamReady, // This flag notifies the server the client streaming socket is ready listening
    LocalViewParams([ViewParams; 2]), // In relation to head
    Battery(BatteryInfo),
    Buttons(Vec<ButtonEntry>),
    ActiveInteractionProfile {
        device_id: u64,
        profile_id: u64,
        input_ids: HashSet<u64>,
    },
    Log {
        level: LogSeverity,
        message: String,
    },
    ProximityState(bool),
    Reserved(String),
    ReservedBuffer(Vec<u8>),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum FaceExpressions {
    Fb(Vec<f32>), // 70 values
    Bd(Vec<f32>), // 52 values
    Htc {
        eye: Option<Vec<f32>>, // 14 values
        lip: Option<Vec<f32>>, // 37 values
    },
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct FaceData {
    /// Head-local orientation whose -Z axis follows the combined gaze direction.
    /// The sample is associated with TrackingData::poll_timestamp, not an independent gaze clock.
    pub eyes_combined: Option<Quat>,
    /// Head-local per-eye orientations, potentially smoothed by the runtime for social presence.
    /// Also used as a foveation input fallback when native combined gaze is unavailable.
    pub eyes_social: [Option<Quat>; 2],

    pub face_expressions: Option<FaceExpressions>,
}

#[derive(Serialize, Deserialize)]
pub struct TrackingData {
    pub poll_timestamp: Duration,
    pub device_motions: Vec<(u64, DeviceMotion)>,
    pub hand_skeletons: [Option<[Pose; 26]>; 2],
    pub face: FaceData,
    pub body: Option<BodySkeleton>,
}

#[derive(Serialize, Deserialize)]
pub struct VideoPacketHeader {
    pub timestamp: Duration,
    pub global_view_params: [ViewParams; 2],
    /// Centers used to encode this frame, already aligned. Normally absent when FFR is disabled.
    pub foveation_center_shifts: Option<[[f32; 2]; 2]>,
    pub is_idr: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Haptics {
    pub device_id: u64,
    pub duration: Duration,
    pub frequency: f32,
    pub amplitude: f32,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum PathSegment {
    Name(String),
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSegment::Name(name) => write!(f, "{name}"),
            PathSegment::Index(index) => write!(f, "[{index}]"),
        }
    }
}

impl Debug for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl From<&str> for PathSegment {
    fn from(value: &str) -> Self {
        PathSegment::Name(value.to_owned())
    }
}

impl From<String> for PathSegment {
    fn from(value: String) -> Self {
        PathSegment::Name(value)
    }
}

impl From<usize> for PathSegment {
    fn from(value: usize) -> Self {
        PathSegment::Index(value)
    }
}

// A name segment + a (possibly empty) list of `[index]` segments.
static COMPOUND_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+)((?:\[[0-9]+\])*)").unwrap());

/// Parses a settings path like `"video.foveated_encoding.enable"` or
/// `"headset.controllers.content.gestures[2].joystick_range"` into name/index segments.
pub fn parse_path(path: &str) -> Result<Vec<PathSegment>> {
    fn parse_error(path: &str, rest: &str, expected: &str) -> Error {
        let found = rest
            .chars()
            .next()
            .map_or_else(|| "end of input".to_owned(), |c| format!("`{c}`"));
        Error::msg(format!(
            "invalid path \"{path}\" at column {}: expected {expected}, found {found}",
            path.len() - rest.len() + 1,
        ))
    }

    let mut segments = Vec::new();
    let mut rest = path;

    loop {
        let Some(caps) = COMPOUND_SEGMENT.captures(rest) else {
            return Err(parse_error(path, rest, "a name `[A-Za-z0-9_]+`"));
        };
        segments.push(PathSegment::Name(caps[1].to_owned()));
        for index in caps[2].split(['[', ']']).filter(|s| !s.is_empty()) {
            match index.parse() {
                Ok(index) => segments.push(PathSegment::Index(index)),
                Err(_) => {
                    let at = &rest[caps[1].len()..];
                    return Err(parse_error(
                        path,
                        at,
                        &format!("index `{index}` to fit in a usize"),
                    ));
                }
            }
        }
        rest = &rest[caps[0].len()..];

        if rest.is_empty() {
            return Ok(segments);
        }
        rest = rest
            .strip_prefix('.')
            .ok_or_else(|| parse_error(path, rest, "`.` or end of input"))?;
    }
}

pub fn path_to_string(path: &[PathSegment]) -> String {
    let mut string = String::new();
    for (i, segment) in path.iter().enumerate() {
        if i != 0 && matches!(segment, PathSegment::Name(_)) {
            string.push('.');
        }
        let _ = write!(string, "{segment}");
    }
    string
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientConnectionsAction {
    AddIfMissing {
        trusted: bool,
        manual_ips: Vec<IpAddr>,
    },
    SetDisplayName(String),
    Trust,
    SetManualIps(Vec<IpAddr>),
    RemoveEntry,
    UpdateCurrentIp(Option<IpAddr>),
    SetConnectionState(ConnectionState),
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ClientStatistics {
    pub target_timestamp: Duration, // identifies the frame
    pub frame_interval: Duration,
    pub video_decode: Duration,
    pub video_decoder_queue: Duration,
    pub rendering: Duration,
    pub vsync_queue: Duration,
    pub total_pipeline_latency: Duration,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PathValuePair {
    pub path: Vec<PathSegment>,
    pub value: json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FirewallRulesAction {
    Add,
    Remove,
}

// Note: server sends a packet to the client at low frequency, binary encoding, without ensuring
// compatibility between different versions, even if within the same major version.
#[derive(Serialize, Deserialize, PartialEq, Clone)]
pub struct RealTimeConfig {
    pub passthrough: Option<PassthroughMode>,
    pub clientside_post_processing: Option<ClientsidePostProcessingConfig>,
    pub cpu_performance_level: Option<PerformanceLevel>,
    pub gpu_performance_level: Option<PerformanceLevel>,
    pub ext_str: String,
}

impl RealTimeConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            passthrough: settings.video.passthrough.clone().into_option(),
            clientside_post_processing: settings
                .video
                .clientside_post_processing
                .clone()
                .into_option(),
            cpu_performance_level: settings.headset.performance_level.clone().cpu.into_option(),
            gpu_performance_level: settings.headset.performance_level.clone().gpu.into_option(),
            ext_str: String::new(), // No extensions for now
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PathSegment, parse_path, path_to_string};

    fn debug_path(path: &str) -> String {
        format!("{:?}", parse_path(path).unwrap())
    }

    #[test]
    fn parses_names_only() {
        assert_eq!(debug_path("my.element.content"), "[my, element, content]");
    }

    #[test]
    fn parses_index_between_names() {
        let segments = parse_path("my.element[1].content").unwrap();
        assert!(matches!(
            segments.as_slice(),
            [
                PathSegment::Name(a),
                PathSegment::Name(b),
                PathSegment::Index(1),
                PathSegment::Name(c),
            ] if a == "my" && b == "element" && c == "content"
        ));
    }

    #[test]
    fn parses_chained_indices() {
        assert_eq!(debug_path("matrix[1][2]"), "[matrix, [1], [2]]");
        assert_eq!(debug_path("a[0][12][345]"), "[a, [0], [12], [345]]");
    }

    #[test]
    fn accepts_underscores_and_digits_in_names() {
        assert_eq!(debug_path("_priv.x1[0]._2y"), "[_priv, x1, [0], _2y]");
    }

    #[test]
    fn rejects_malformed_paths() {
        for path in [
            "",
            ".foo",
            "foo.",
            "foo..bar",
            ".[1]",
            "foo.[1]",
            "[0].foo",
            "foo[]",
            "foo[",
            "foo[1",
            "foo[1]bar",
            "foo[-1]",
            "foo[1.5]",
            "foo[key]",
            "foo-bar",
            "foo bar",
            "foo.bar!",
            "foé",
        ] {
            assert!(
                parse_path(path).is_err(),
                "expected \"{path}\" to be rejected"
            );
        }
    }

    #[test]
    fn rejects_index_overflow() {
        assert!(parse_path("foo[99999999999999999999999999]").is_err());
    }

    #[test]
    fn path_to_string_round_trips() {
        for path in [
            "a",
            "a.b.c",
            "a[0]",
            "a[1][2].b",
            "video.foveated_encoding.strength",
            "headset.controllers.content.gestures[2].joystick_range",
        ] {
            assert_eq!(path_to_string(&parse_path(path).unwrap()), path);
        }
    }

    #[test]
    fn error_points_at_the_failing_column() {
        assert_eq!(
            parse_path("foo..bar").unwrap_err().to_string(),
            "invalid path \"foo..bar\" at column 5: expected a name `[A-Za-z0-9_]+`, found `.`",
        );
        assert_eq!(
            parse_path("foo[1]x").unwrap_err().to_string(),
            "invalid path \"foo[1]x\" at column 7: expected `.` or end of input, found `x`",
        );
        assert_eq!(
            parse_path("foo[]").unwrap_err().to_string(),
            "invalid path \"foo[]\" at column 4: expected `.` or end of input, found `[`",
        );
    }
}
