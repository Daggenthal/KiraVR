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

### Run 1 (2026-11-06, Beat Saber main menu, head still, 60 s each)

| Test | Total | Decode queue | Vsync | Headset fps (low) | Slow |
|---|---|---|---|---|---|
| Baseline (72 Hz, H.264, buffering 2.0) | 49.8 | 9.6 | 18.7 | 72 (72) | 0 % |
| Buffering 1.0 | 47.9 | 6.3 | 19.3 | 72 (72) | 0.4 % |
| Buffering 1.5 | 46.9 | 4.3 | 19.0 | 72 (72) | 0.1 % |
| 90 Hz | 61.9 | 1.2 | 38.0 | 79 (45) | 25 % |
| 120 Hz | 47.9 | 1.9 | 23.9 | 80 (60) | 66 % |
| H.264 (same as baseline) | 54.3 | 13.5 | 18.7 | 72 (72) | 0.1 % |
| HEVC | 56.7 | 13.9 | 19.6 | 72 (72) | 0.5 % |
| AV1 (two runs) | 53.2 / 56.0 | 11.2 / 13.2 | 18.8 | 72 (72) | 0.7 / 0.1 % |

Times in ms. Network was a steady 3 ms, encode 8 ms (AV1 7 ms), decode 6.4 ms (H.264), 7.7 (HEVC), 8 (AV1).

What it shows:

1. **The decode queue varies run to run with the same settings** (9.6 vs 13.5 ms). That is the free-running Linux grid drifting against the headset. The buffering differences are about as large as that noise, so they do not prove much on their own. Locking the grid to the headset is the real fix.
2. **90 and 120 Hz did not deliver more frames.** MangoHud showed the game at 90 and 120 fps, and the headset ran at 90 and 120 Hz (the "low" values are exactly half the refresh: repeated frames). But only about 72 new frames per second reached the headset (4321 and 4270 samples in 60 s, one sample per new frame shown). The "fps" column was misleading: It averaged per-frame values. Suspected cause: The Linux encode mailbox keeps only the newest frame ("latest wins") and silently drops the rest when the encoder is busy, so the encoder's throughput at this resolution (SteamVR renders 3216x3360) would be the limit.
3. **H.264 is fine.** It decodes fastest; the totals are within the noise.

Recommendation until the code changes: 72 Hz, H.264, buffering 1.5.

### Next run: Where do the 90 Hz frames go?

The driver now counts frames dropped from the encode mailbox (`ReportEncodeJob`, logged every 10 s as "Streamer dropped N of M frames", shown as "Dropped by streamer" in the overlay and the Lab). The Lab records streamer fps, new frames per second on the headset and the share of repeated frames, and has resolution steps.

1. Baseline at 72 Hz.
2. 90 Hz at full resolution: Expect "Dropped by streamer" around 18/s if the encoder is the limit.
3. 90 Hz at 85 % and 75 % resolution: If new frames per second rise toward 90 and the drops fall, encoder throughput is proven, and the fix is encoder settings for throughput plus sensible defaults for high refresh rates.
