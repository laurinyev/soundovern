# soundovern

Stream **all desktop audio** (PipeWire default-sink monitor) to an Android phone over the LAN
with low latency. Rust server, Opus over UDP, adaptive jitter buffer, Kotlin app.

## Status
| Piece | State |
|---|---|
| PipeWire capture (`soundovern-server`) | working, verified on live PipeWire 1.6 |
| Opus encode + UDP + TCP control + mDNS advert | working |
| Receiver engine (`soundovern-core::client`): jitter buffer, FEC/PLC, drift resampler, auto-reconnect | working, unit-tested, verified via loopback with simulated loss and a server restart |
| Desktop test client (`soundovern-cli-client`) | working (cpal playback or `--wav`) |
| Android app (`android/`) + JNI bridge | built and tested on a real phone over Wi-Fi: connects, streams, plays; latency tuning ongoing |

## Quick start (desktop)
```sh
cargo run --release -p soundovern-server            # prints the address to connect to
cargo run --release -p soundovern-cli-client -- 192.168.x.y   # on another machine (a local client would feed back into the capture!)
./tools/loopback-test.sh 5                          # headless self-test with 5 % simulated loss
cargo run -p soundovern-server -- --probe 3 --wav /tmp/p.wav   # capture-only sanity check
```
Options: `--silent-local` (mute desktop output while a client is connected; restores on disconnect/exit), `--pin 1234`, `--bitrate`, `--frame-ms 5|10|20`, `--target <pw node>` (instead of default sink), `--no-fec`, `--bind`.
Ports: TCP 7420 (control) and an ephemeral UDP port on the client (media, server→client). Open TCP 7420 on the desktop firewall.
Optional: `packaging/soundovern.service` as a systemd *user* unit.

## Android build
Needs the Android SDK (platform 35, build-tools, NDK 27), a **JDK 17** (JDK 27 is too new for this Gradle/Kotlin; a JRE alone lacks `javac`), `cargo-ndk`, and Gradle ≥ 8.9 once to create the wrapper.
```sh
export ANDROID_HOME=$HOME/Android/Sdk ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018 JAVA_HOME=/path/to/jdk17
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -o android/app/src/main/jniLibs build --release -p soundovern-android-ffi
cd android && echo "sdk.dir=$ANDROID_HOME" > local.properties && ./gradlew assembleDebug
# APK: android/app/build/outputs/apk/debug/app-debug.apk
```
Playback uses Oboe (AAudio, exclusive/MMAP when the device allows; OpenSL ES on old phones) with the callback in Rust. The device buffer starts at 2 bursts and grows one burst per xrun; the stream reopens on route changes.

## Tips
- **Echo:** playing the same audio on desktop speakers and the phone sounds like an echo (the phone is ~50–100 ms later). Use `--silent-local`, or mute the desktop.
- **USB instead of Wi-Fi:** enable USB tethering on the phone (no USB debugging needed), find the desktop's address on the new interface with `ip -4 addr`, and connect to that `IP:7420`. adb-based tunnels carry only TCP, so they are not suitable.
- **Bluetooth headphones** on the phone add 100–200 ms on their own.

## Layout
`crates/soundovern-proto` wire format · `soundovern-core` codec/jitter/drift/client engine · `soundovern-server` · `soundovern-cli-client` · `soundovern-android-ffi` · `android/` app.
Protocol: UDP datagram = 19-byte header (magic, ver, flags, session, seq, 48 kHz timestamp, len) + one Opus frame; control = length-prefixed postcard over TCP. See `crates/soundovern-proto/src/lib.rs`.

## Known gaps / next steps
- QR pairing; media encryption (PIN is checked in plaintext today – LAN-grade only).
- Single client at a time (a new client replaces the old one).
- Latency not yet measured glass-to-glass (see plan: chirp cross-correlation rig).
