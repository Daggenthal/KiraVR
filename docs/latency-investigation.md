# Latency Investigation

Started 2026-11-06. Goal: Lower the end-to-end latency on Linux without adding stutter.

## Where the time goes

Baseline from the in-VR overlay (Full mode), Quest 3 at 72 Hz, HEVC, wired Ethernet PC, Wi-Fi headset: About **55 ms total**. The two biggest parts are on the headset:

| Part | Typical | What it is |
|---|---|---|
| Decode queue | ~16 ms | A decoded frame waiting for the headset's render loop to pick it up |
| Vsync queue | ~18 ms | The headset runtime's own lead: From handing the frame over to it being on the display |
| Network | ~3 ms | Small, the network is not the problem |

## Why

**Decode queue.** Decoded frames go into a FIFO (`client_core/src/video_decoder/android.rs`). The render loop (`client_openxr/src/stream.rs`, `render`) takes the oldest frame each display frame. Old frames are only dropped once a running average of the queue length passes **Maximum buffering** (default 2.0 frames, history weight 0.90). Each frame that waits costs one frame interval (13.9 ms at 72 Hz), so ~16 ms means about one frame is always waiting.

**Why frames pile up on Linux.** On Windows the streamer paces frames to the headset's display (`WaitForVSync`, using `duration_until_next_vsync` from the statistics). On Linux, direct mode paces against its own free-running grid (`server_openvr/src/vsync.rs`) that knows the refresh rate but not the headset's phase. Frames arrive at a random, slowly drifting point in the headset's frame, so the client keeps a frame of slack to avoid running dry. Upstream left a "proper phase sync" TODO for this. `Enforce server frame pacing` only drives the Windows path, so it is correctly hidden on Linux.

**Vsync queue.** Reported by the runtime (predicted display time minus now at submit). Mostly fixed by the Quest, but counted in frames, so it shrinks in milliseconds at higher refresh rates.

## Test plan

Run it from the dashboard's **Latency Lab** tab: It shows the live breakdown, has every setting below in one place, applies each step with one click (starting from your own settings, which it remembers and can restore), measures 60 seconds and keeps the results as a table you can copy into this file.

Same scene for every run (a static spot in a game, or the SteamVR dashboard), head still, 60 s per setting. Turn on Log to Disk so the numbers can also be read from `~/alvr_session_log.txt` afterwards. Note the clock time of each run. Change one thing at a time.

| # | Change | Applies after | Expect | Watch for |
|---|---|---|---|---|
| 0 | Baseline (defaults) | | ~55 ms | Write down every overlay value |
| 1 | Maximum buffering 2.0 → 1.0 | Reconnect | Decode queue down by ~7 to 14 ms | Stutter, received fps below refresh |
| 2 | Maximum buffering 1.5 | Reconnect | Between 0 and 1 | Same |
| 3 | Refresh 72 → 90 Hz (best buffering from 1/2) | SteamVR restart | Frame-based parts down ~20 % | GPU and encoder keeping 90 fps |
| 4 | Refresh 120 Hz | SteamVR restart | Down ~40 % | Same, plus bitrate |
| 5 | Codec H.264 / AV1 instead of HEVC | SteamVR restart | Decode time differs | Image quality at the same bitrate |

Settings: Video, Maximum buffering (and Buffering history weight next to it); Video, Preferred FPS; Video, Preferred codec.

## Code changes to consider after measuring

1. **Phase-lock the Linux grid to the headset.** Nudge the grid's next tick so frames arrive a little before the headset's render loop needs them, using the client vsync timing the statistics already track (`duration_until_next_vsync`). Expected: The decode queue settles near the decode time alone, and buffering can stay low without stutter. This is the real fix, and the biggest change.
2. **Newest frame instead of oldest.** When more than one frame waits, show the newest and drop the rest. Cuts latency at once, but trades it for visible skips when the network hiccups. Only with a setting.
3. **Better defaults** from the measurements (buffering, refresh rate) if one setup is clearly better on Quest 3.

## Results

(Filled in during testing.)
