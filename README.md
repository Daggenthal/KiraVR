<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="resources/kiravr/png/kiravr-wordmark-dark.png">
    <img width="450" alt="KiraVR" src="resources/kiravr/png/kiravr-wordmark-light.png">
  </picture>
</p>

> [!IMPORTANT]
> **This is KiraVR, an unofficial fork of [ALVR](https://github.com/alvr-org/ALVR).** 🐧
> The goal: Keep every setting ALVR has, make it work reliably with SteamVR on Linux and SteamOS, and make setup simpler.
> KiraVR is not affiliated with or endorsed by the ALVR team. Please report KiraVR problems [here](https://github.com/Daggenthal/KiraVR/issues), not to ALVR.
> The Android client installs as `dev.daggenthal.kiravr` and shows up as "KiraVR", so it can sit next to an installed ALVR.

> [!NOTE]
> **AI usage:** KiraVR is developed with help from AI coding tools (Claude). Every change is reviewed and tested on real hardware before it is committed. Upstream ALVR does not accept AI-assisted contributions, so code from this fork is not sent back to ALVR.

Everything below is the original ALVR README.

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
