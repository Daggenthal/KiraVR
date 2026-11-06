# Baseline: Upstream ALVR v21.0.0-dev14 on SteamOS

Tested 2026-11-06 to see what upstream direct mode does on our hardware before KiraVR changes anything.

## Setup

| | |
|---|---|
| Host | Steam Machine, AMD Navi 33 (RADV, Mesa 26.2), SteamOS, KDE Plasma (Wayland), 16 GB RAM |
| Streamer | Upstream nightly v21.0.0-dev14 (commit e8ca92b), `alvr_streamer_linux` tarball |
| Client | Quest 3, `alvr.client.dev` 21.0.0-dev14+nightly.2026.11.06 |
| SteamVR | 2.18.4 (build 25998080) |
| Network | Wi-Fi 6 on both ends at first, later host on gigabit Ethernet, also tested wired (USB/adb) |
| Game | Beat Saber (Proton Experimental, OpenXR) |

## Summary

| Area | Result |
|---|---|
| Discovery, trust, first connection | ✅ Works |
| Tracking, controllers, hand tracking | ✅ Works |
| Video (VAAPI, H.264) | ✅ Works, see stutter below |
| SteamVR dashboard overlay and input | ✅ Works (once SteamVR Home is off) |
| Game audio and microphone | 🟡 Work, but must be selected by hand every session |
| Stutter when a game starts | ❌ Encoder rebuilt on every layer 0 texture set switch |
| Reconnect during a session | ❌ Intermittent vrserver crash (same root cause) |
| "Add firewall rules" button | ❌ Exit 126, packaging bug |
| SteamVR with `usePrism` on | ❌ Restart loop, error 156 |
| SteamVR Home | ❌ Never loads under the new Steam client (Valve bug) |
| `vrmonitor.sh` launch option | 🟡 Not needed, and a wrong value crashes Steam |
| Wired toggle in the dashboard | ❌ Gets stuck on "Disconnecting" |
| Logs | 🟡 Off by default, wrong filename in docs, stats are empty |
| v20 client against the v21 streamer | ❌ Silently ignored |

## Findings

### 1. Stutter on game start (ALVR, fixed in KiraVR)

Symptom: Choppy video for 10 to 20 seconds after launching a game, also on Ethernet and wired. Opening the SteamVR menu once or twice made it go away.

Cause: `OvrDirectModeComponent::Present` binds the encode pipeline to the swap texture set that layer 0 comes from. While a game starts, SteamVR switches layer 0 between its own scene (3216x3360) and the game's swapchain (2368x2472). Every switch rebuilt the renderer and the whole VAAPI encoder, including a new IDR frame.

Evidence: Every encoder rebuild in `~/alvr_session_log.txt` matched a `WaitForAcquire timed out` warning in `vrcompositor.txt` to within a few milliseconds. The Quest decoder dropped from 72 to 55 fps in the window with two rebuilds. SteamVR never sent throttle hints, so throttling was ruled out.

Fix (commit 8d7ced50): The new renderer adopts the old output image when its size and format still fit, so the encoder keeps running. Verified: Five texture set switches during Beat Saber startup, all logged as "kept the running encoder", no stutter.

### 2. vrserver crash on reconnect (ALVR, fixed in KiraVR)

Symptom: SteamVR crashed on some reconnects. On the next start, safe mode blocked the `alvr_server` driver.

Cause: Same code path as finding 1. `Encoder::createImages` destroyed the renderer, and with it the output image, while the encoder still referenced that image. `initEncoding` then replaced the encoder, and its destructor flushed FFmpeg against freed memory.

Evidence: Three minidumps, identical stack: `OvrDirectModeComponent::Present` → `Encoder::initEncoding` → `~EncodePipelineVAAPI` (`EncodePipelineVAAPI.cpp:399`) → `avcodec_send_frame` → `ff_hw_base_encode_receive_packet` (`hw_base_encode.c:581`), SIGSEGV at 0x28. It was intermittent: Some reconnects survived.

Fix (commit 8d7ced50): The encoder and frame are destroyed before the image they use. Verified: Reconnect during a session, SteamVR stayed up.

### 3. Audio devices not switched (ALVR, fixed in KiraVR)

On Windows ALVR reports its devices to SteamVR through the HMD audio properties and SteamVR switches the defaults. On Linux that code is compiled out, so "ALVR Audio" and "ALVR Microphone" had to be picked by hand every session.

Fix (commit deaa130e): New Linux setting "Switch system audio to the headset" (on by default). It sets the PipeWire `default.configured.audio.sink/source` metadata while streaming and restores the previous values afterwards, falling back to the effective default if the saved value is a stale ALVR name.

### 4. Firewall button fails with exit 126 (ALVR, fixed in KiraVR)

`xtask` created a directory named `alvr_fw_config.sh` and put the script inside it, so the dashboard tried to run a directory. The script's own comment says 126 means "pkexec request dismissed", which is misleading here. On SteamOS this is harmless anyway: The `public` zone already allows ports 1024 to 65535, and SteamOS ships an `alvr` firewalld service. On other distros the firewalld path also depends on that service existing, since the script never installs `alvr-firewalld.xml`.

Fix (commit c15ce6c8). Verified: The `alvr` service was added to the `public` zone.

### 5. SteamVR restart loop with `usePrism` (Valve, workaround)

With `"usePrism": true` in `~/.local/share/Steam/config/steamvr.vrsettings`, `vrprismhost` never starts while ALVR is the HMD driver. Clients log `ConnectToPrism: prism process not found`, vrmonitor reports `SteamVR Fail (156)` (`VRInitError_Init_PrismExitedUnexpectedly`) and restarts everything every two minutes. Setting it to `false` fixed it. Preflight candidate.

### 6. SteamVR Home never loads (Valve, workaround)

`steamtours.sh` hardcodes `$HOME/.steam/bin/steam-runtime/run.sh`. Since the steamrt3 Steam client (around 2026-10-14), `~/.steam/bin` points to `steamrt32`, which has no `steam-runtime/`. SteamVR retries Home every 200 ms forever, and the system menu does not take input while that loop runs. Turning off SteamVR Home fixes both. Not related to ALVR. Preflight candidate.

### 7. `vrmonitor.sh` launch option (ALVR docs)

The dashboard tells users on every launch to add `.../SteamVR/bin/vrmonitor.sh %command%` to SteamVR's launch options, without checking. SteamVR already starts vrmonitor through `vrenv.sh`, so it is not needed. When the value was not the full path, `vrmonitor` never started, vrserver gave up after 25 seconds and sent Quit to every client, including the Steam client itself, which then crashed in a loop.

### 8. Stuck states and leftovers

- After a crash or a manual close, `vrcompositor` and Steam's launch wrapper (`reaper SteamLaunch AppId=250820`) often kept running, so SteamVR looked "stuck closing" and could not be relaunched until they were killed.
- Turning Wired off and on in the dashboard dropped the wireless stream and left `client.wired` in `Disconnecting` until the dashboard was restarted.
- The dashboard's wired mode restarts the adb server, which kills any other adb session such as `adb logcat`.

### 9. Logging gaps

- `log_to_disk` is off by default and only takes effect after SteamVR restarts.
- On Linux the file is `~/alvr_session_log.txt`, not `session_log.txt`. Errors also go to `~/crash_log.txt`.
- `[STATS]` and `[GRAPH]` lines in the log file are empty. The numbers only exist in the dashboard.
- `GetFrameTiming` always reports 0 dropped frames to SteamVR on purpose (to avoid interleaved reprojection), so SteamVR's own frame stats cannot show drops on this driver.

### 10. Two client apps

The Quest had both `alvr.client` (v20.14.0) and `alvr.client.dev` (v21 nightly). Both show up as "ALVR" in Unknown Sources with no version. The v21 streamer ignored the v20 client without any message. KiraVR's own app ID and a clear version mismatch message address this.

## Environment notes

- Wi-Fi on both the PC and the headset caused short snags of its own. Gigabit Ethernet on the PC removed them.
- lsfg-vk (Lossless Scaling frame generation) is installed as an implicit Vulkan layer and loads into `vrcompositor` and every other Vulkan process. Its profiles only cover two non-VR games, so it was inactive here, but a matching profile would affect VR. Preflight candidate.
- SteamVR's built-in Steam Link driver (`driver_vrlink`) loads next to ALVR and adds phantom `VRLINKQ_Hand_*` devices. No visible effect seen.
- The encoder picked H.264, not HEVC, on this setup. Not investigated yet.
- A parallel build of the streamer during a VR session filled RAM and swap and the OOM killer took the game. `scripts/build-streamer-distrobox.sh` now limits jobs.

## Where the evidence is

| What | Where |
|---|---|
| ALVR session log | `~/alvr_session_log.txt`, `~/crash_log.txt` |
| SteamVR logs | `~/.local/share/Steam/logs/vrserver.txt`, `vrcompositor.txt`, `vrmonitor.txt` (only the current and previous run are kept) |
| Steam launch output | `~/.local/share/Steam/logs/console-linux.txt`, `vrstartup-linux.txt` |
| Minidumps | `/tmp/dumps/crash_*.dmp` (lost on reboot) |
| Headset log | `adb logcat`, the `QC2Comp ... Stats` lines give the decoded frame rate every 5 seconds |
