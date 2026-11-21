# Privacy

*Effective 2026-11-21. This describes what the Soundovern Android app and desktop server actually do.*

**Short version:** Soundovern has no accounts, no analytics, no ads, no crash reporting and no
servers of its own. Nothing is sent to the developers or to any third party. The app talks only to
the desktop computer you point it at, on your own network.

## What the app does with data

- **Audio.** The desktop server captures the sound your computer is playing and sends it to the app,
  which plays it. The audio is encrypted in transit with a key derived from your pairing PIN. It is
  never recorded or saved, and it goes nowhere except to your phone.
- **Network.** The app connects only to the address you enter (or one it finds on your local network).
  To find desktops it uses standard local-network discovery (mDNS), which sends and receives small
  messages on your Wi-Fi/LAN and does not leave it. It does not contact the internet.
- **Device name.** When connecting, the app tells your desktop the phone's model name (for example
  "Pixel 8") so the server can show who is connected. This stays on your network.

## What is stored on your phone

Stored privately inside the app (other apps cannot read it, and it is excluded from Android backups):

- the desktop address you entered,
- the pairing PIN you entered (saved so you do not have to retype it),
- the volume setting,
- a flag recording that you have seen the "Made with AI" notice.

The address and PIN are saved as plain values in the app's private storage. Clearing the app's data
or uninstalling it removes everything.

## Logs

The app writes diagnostic messages to Android's system log (viewable over `adb logcat` with the tag
`soundovern`). They include the desktop address and connection statistics (packet counts, delay
figures) but not audio and not your PIN. They are not uploaded anywhere. Anyone with USB debugging
access to your phone can read them.

## Permissions the app asks for, and why

| Permission | Why |
|---|---|
| Network (`INTERNET`, `ACCESS_NETWORK_STATE`, `ACCESS_WIFI_STATE`) | Connect to your desktop and discover it on the LAN. |
| `CHANGE_WIFI_MULTICAST_STATE` | Local-network discovery. |
| Foreground service (media playback) and notifications | Keep playing audio with the screen off and show a Disconnect/Mute notification. |
| `WAKE_LOCK` | Stop the phone sleeping the Wi-Fi radio or the CPU while streaming. |

The app does not request access to your contacts, location, microphone, camera, storage or accounts.

## The desktop server

- Stores the pairing PIN in `~/.config/soundovern/pin` (readable only by your user).
- Logs, on your computer only, the model name and IP address of connected devices and connection
  statistics.
- Advertises its name on your local network via mDNS so apps can find it.
- Does not contact the internet or send data anywhere.

## Third parties

No third party receives any data from Soundovern. The software includes open-source libraries
(listed in `THIRD_PARTY_LICENSES.txt`); none of them collect data.

## Children

Soundovern is a general-purpose utility and collects no personal information from anyone.

## Changes and contact

If this changes, the new version will be published in this repository with a new effective date, and
the app's source will show what changed. Questions or concerns: open an issue in this project's
repository.

*This notice is provided in plain language for transparency; it is not legal advice.*
