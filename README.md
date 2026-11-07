<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="resources/kiravr/png/kiravr-wordmark-dark.png">
    <img width="450" alt="KiraVR" src="resources/kiravr/png/kiravr-wordmark-light.png">
  </picture>
</p>

<p align="center"><b>Stream SteamVR from your PC to a standalone headset. Built for Linux and SteamOS.</b></p>

<p align="center">
  <a href="https://github.com/Daggenthal/KiraVR/releases"><b>⬇️ Download</b></a> ·
  <a href="#-install">Install</a> ·
  <a href="#-whats-different-from-alvr">What's different</a> ·
  <a href="#-troubleshooting">Troubleshooting</a> ·
  <a href="#-building-from-source">Build</a>
</p>

KiraVR is a fork of [ALVR](https://github.com/alvr-org/ALVR). It keeps every setting ALVR has and focuses on making SteamVR on Linux and SteamOS work reliably, with a setup that does not need a terminal.

> [!IMPORTANT]
> KiraVR is not affiliated with or endorsed by the ALVR team. Please report KiraVR problems [here](https://github.com/Daggenthal/KiraVR/issues), not to ALVR.
> The headset app installs as `dev.daggenthal.kiravr` and shows up as "KiraVR", so it can sit next to an installed ALVR. The two do not talk to each other: Use the KiraVR app with the KiraVR streamer.

## ✨ What's different from ALVR

- **Built on SteamVR's official driver API on Linux:** KiraVR builds on ALVR's new direct mode (no `vrcompositor` wrapper) and fixes what broke in daily use: Stutter when a game starts, crashes on reconnect, a wired connection stuck on "Disconnecting", and SteamVR blocking the driver after a slow shutdown.
- **System Check:** The dashboard checks your PC before every launch: Steam and SteamVR setup, known SteamVR settings that break streaming, leftover processes, firewall, GPU access, a real hardware encoder test, Vulkan layers and your network. Most problems have a **Fix** button. Also available as `--preflight` in a terminal.
- **Install on Headset:** Plug in the headset, click once, and the dashboard installs the matching app, with live progress. It downloads adb by itself if needed.
- **AppImage:** One file that runs on most Linux distributions and adds itself to the app menu.
- **In-VR performance overlay:** FPS, latency, bitrate, packet loss and the headset battery (with power draw and time left), right inside the headset. Hold both thumbsticks to show it, pin it to your wrist or grab it with a laser pointer to move it.
- **Audio follows the headset:** While streaming, your PC's sound and microphone switch to the headset and switch back afterwards.
- **Diagnostics for bug reports:** One click collects versions, hardware, SteamVR, headset, changed settings and recent logs, with your user name, PC name, addresses and serial numbers removed. You read it before sharing it.
- **No forced updates:** Nothing updates by itself. New versions are only installed after you confirm twice, the last 3 versions are kept, and you can switch back with a click.
- **Clear version messages:** If the headset app and the PC do not match, both sides say so and tell you what to update.

## 📦 Install

### What you need

- A PC with SteamVR, running Linux (SteamOS, Arch, Fedora 40 or newer, Ubuntu 24.04 or newer). An AMD GPU is the tested setup, NVIDIA and Intel should work but are untested.
- A Quest 3 (tested). Other Quest headsets and the headsets ALVR supports should work.
- The PC on wired Ethernet and the headset on 5 GHz Wi-Fi, on the same router. Wi-Fi on both ends works, but expect short hitches.

### Steps

1. **Download** `KiraVR-<version>-x86_64.AppImage` from [Releases](https://github.com/Daggenthal/KiraVR/releases).
2. **Start it:** Right-click it, Properties, allow it to run as a program, then double-click. From a terminal: `chmod +x KiraVR-*.AppImage && ./KiraVR-*.AppImage`. KiraVR now shows up in your app menu.
3. **Check your PC:** Open the **Installation** tab and expand **System Check**. Fix anything it marks.
4. **Put the app on the headset:**
   1. Turn on developer mode for your headset (for Quest: In the Meta Horizon phone app, under Headset Settings, Developer Mode).
   2. Connect it to the PC with a USB cable and allow USB debugging inside the headset.
   3. In **Installation, Headset App**, click **Install on Headset**.
5. **Connect:** Start KiraVR on the headset (Library, Unknown Sources), click **Trust** next to it in the dashboard, then **Launch SteamVR**.

### Updates are your choice

KiraVR never downloads or installs anything by itself.

- **Checking:** The dashboard can ask GitHub at startup whether a new version exists, and only shows a notice. Turn it off under Installation, Updates (or Settings, Extra, Check for Updates on Start), and KiraVR does not contact GitHub at all. **Check for Updates** in the same place checks once when you click it.
- **Updating:** Download the new AppImage and start it. It asks twice before it replaces your installed version. Say no and it keeps your version, and it will not ask again for that build (`--update` asks again).
- **Going back:** The last 3 versions are kept. Switch back under Installation, Updates, **Switch to This Version**. If a new version does not even start, run `./KiraVR-*.AppImage --rollback`. Both ask twice too.
- **Headset app:** Updating or going back on the headset is also confirmed twice, and going back warns that the app's settings may be reset.

### Windows

Windows builds are made automatically with every release, but nobody has tested them yet. Use them if you are curious, and please report what happens.

### Uninstall

Run `./KiraVR-*.AppImage --uninstall`. It removes the installed copy, the kept older versions, the menu entry and the SteamVR driver registration. Settings stay in `~/.config/alvr`. Remove the headset app in the headset's app library.

## 🩺 Troubleshooting

- **Start with System Check** in the Installation tab, or run `./KiraVR-*.AppImage --preflight` in a terminal.
- **Logs:** Turn on Settings, Extra, Logging, **Log to disk**, then restart SteamVR. The log is `~/alvr_session_log.txt`. Errors always go to `~/crash_log.txt`. SteamVR's own logs are in `~/.local/share/Steam/logs/`.
- **SteamVR keeps restarting every two minutes:** Set `"usePrism": false` in SteamVR's settings (System Check does this for you).
- **SteamVR Home hangs or the menu does not react:** Turn SteamVR Home off (System Check does this too).
- **Choppy stream:** Connect the PC with a cable and keep the headset near the router. The performance overlay shows packet loss and where the latency comes from.

**Reporting a problem:** Open the **Diagnostics** tab, click **Collect Diagnostics**, read it over and paste it into a [new issue](https://github.com/Daggenthal/KiraVR/issues/new?template=bug_report.yml). Without a working dashboard: `./KiraVR-*.AppImage --diagnostics > diagnostics.md`.

## 🔧 Building from source

On SteamOS or any distribution with [distrobox](https://distrobox.it), the scripts set up an Ubuntu 24.04 build box under your home folder:

```bash
git clone --recurse-submodules https://github.com/Daggenthal/KiraVR.git
cd KiraVR
scripts/build-streamer-distrobox.sh          # PC side, into build/alvr_streamer_linux
scripts/build-client-distrobox.sh --release  # headset app, into build/alvr_client_android
scripts/build-appimage.sh                    # one AppImage from both
```

`KIRAVR_DEBUG=1` logs every step, `NO_COLOR=1` turns colors off, `KIRAVR_JOBS` sets the number of compile jobs. Releases are built by GitHub Actions from a `kiravr-v<version>` tag.

## 🤖 AI usage

KiraVR is developed with help from AI coding tools (Claude). Every change is reviewed and tested on real hardware before it is committed. Upstream ALVR does not accept AI-assisted contributions, so code from this fork is not sent back to ALVR.

## 📄 License and credits

KiraVR is licensed under the [MIT License](LICENSE), like ALVR. All credit for the streaming engine, the headset app and the years of work behind them goes to the [ALVR contributors](https://github.com/alvr-org/ALVR/graphs/contributors) and to [polygraphene](https://github.com/polygraphene/ALVR), who started ALVR.

KiraVR apps do not collect any personal data.

<details>
<summary><b>Original ALVR README</b> (click to open)</summary>

<br>

> The text below is ALVR's own README, kept for reference. Its download links, store links and support channels belong to ALVR, not KiraVR.

# ALVR - Air Light VR

[![badge-discord][]][link-discord] [![badge-matrix][]][link-matrix] [![badge-opencollective][]][link-opencollective]

Stream VR games from your PC to your headset over Wi-Fi.  
This is a fork of [ALVR](https://github.com/polygraphene/ALVR).

### Direct download (latest version):
### [Windows Launcher](https://github.com/alvr-org/ALVR/releases/latest/download/alvr_launcher_windows.zip) | [Linux Launcher](https://github.com/alvr-org/ALVR/releases/latest/download/alvr_launcher_linux.tar.gz)

## Compatibility

|          VR Headset          |                                        Support                                         |
| :--------------------------: | :------------------------------------------------------------------------------------: |
|       Apple Vision Pro       |    :heavy_check_mark: ([store link](https://apps.apple.com/app/alvr/id6479728026))     |
|      Quest 1/2/3/3S/Pro      | :heavy_check_mark: ([store link](https://www.meta.com/experiences/7674846229245715) *) |
|     Pico Neo 3/4/4 Ultra     |                                   :heavy_check_mark:                                   |
|    Play For Dream YVR 1/2/MR |                                   :heavy_check_mark:                                   |
| Vive Focus 3/Vision/XR Elite |                                   :heavy_check_mark:                                   |
|     PhoneVR (smartphone)     |     :heavy_check_mark: ** ([repo](https://github.com/PhoneVR-Developers/PhoneVR))      |
|        Android/Monado        |                                   :warning: **                                         |
|           Lynx R1            |                                   :warning: ***                                        |
|          Oculus Go           |                 :x: ([old repo](https://github.com/polygraphene/ALVR))                 |

\* ALVR for Quest 1 is not available through the Meta store.  
\** Works on some smartphones, but has not been extensively tested.
\*** Temporarily removed, last supported on version [20.14.1](https://github.com/alvr-org/ALVR/releases/tag/v20.14.1).

|     PC OS      |                                    Support                                    |
| :------------: | :---------------------------------------------------------------------------: |
| Windows 10/11  | :heavy_check_mark: ([store link](https://store.steampowered.com/app/3312710)) |
| Windows XP/7/8 |                                      :x:                                      |
|     Linux      |                             :heavy_check_mark:****                            |
|     macOS      |                                      :x:                                      |

\**** Please check the wiki for detailed compatibility information.

### Requirements

-   A supported standalone VR headset (see compatibility table above).
-   SteamVR.
-   A high-end gaming PC:
    -   See the OS compatibility table above.
    -   NVIDIA GPU with NVENC support (GTX 1000 series or newer), an AMD GPU with AMF VCE support, or an INTEL GPU with VPL support (Arc, Tiger Lake or newer), with the latest drivers.
    -   On laptops with both an integrated GPU (Intel HD, AMD iGPU) and a dedicated GPU (NVIDIA GTX/RTX, AMD HD/R5/R7), make sure to assign the dedicated GPU (or "high performance graphics adapter") to ALVR and SteamVR for the best performance and compatibility.  
        (NVIDIA: Nvidia Control Panel → 3D Settings → Application Settings; AMD: similar method)

-   Network:
    -   802.11ac 5 GHz Wi-Fi for the headset, and wired Ethernet for the PC is recommended.
    -   The PC and the headset must be connected to the same router (or use a routed connection as described [here](https://github.com/alvr-org/ALVR/wiki/ALVR-v14-and-Above)).

## Installation

Follow the [installation guide](https://github.com/alvr-org/ALVR/wiki/Installation-guide).

## Troubleshooting

-   See the [Troubleshooting](https://github.com/alvr-org/ALVR/wiki/Troubleshooting) page, and [Linux Troubleshooting](https://github.com/alvr-org/ALVR/wiki/Linux-Troubleshooting) if applicable.
-   Configuration recommendations and additional information can be found [here](https://github.com/alvr-org/ALVR/wiki/Information-and-Recommendations).

## Uninstallation

Open `ALVR Dashboard.exe`, go to the `Installation` tab, then press `Remove firewall rules`.  
Close the ALVR window and delete the ALVR folder.

## Build from Source

Follow the [build guide](https://github.com/alvr-org/ALVR/wiki/Building-From-Source).

## License

ALVR is licensed under the [MIT License](LICENSE).

## Privacy Policy

ALVR apps do not directly collect any personal data.

## Donate

If you would like to support this project, you can donate through our [Open Source Collective account](https://opencollective.com/alvr).

[badge-discord]: https://img.shields.io/discord/720612397580025886?style=for-the-badge&logo=discord&color=5865F2 "Join us on Discord"
[link-discord]: https://discord.gg/ALVR
[badge-matrix]: https://img.shields.io/static/v1?label=chat&message=%23alvr&style=for-the-badge&logo=matrix&color=blueviolet "Join us on Matrix"
[link-matrix]: https://matrix.to/#/#alvr:ckie.dev?via=ckie.dev
[badge-opencollective]: https://img.shields.io/opencollective/all/alvr?style=for-the-badge&logo=opencollective&color=79a3e6 "Donate"
[link-opencollective]: https://opencollective.com/alvr

</details>
