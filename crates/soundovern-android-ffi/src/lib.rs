//! JNI bridge: Kotlin `NativeEngine` <-> `soundovern_core::client::Engine`.
//! Build for Android with `cargo ndk -t arm64-v8a -t armeabi-v7a -o android/app/src/main/jniLibs build --release -p soundovern-android-ffi`.

use jni::objects::{JClass, JFloatArray, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::JNIEnv;
#[cfg(target_os = "android")]
mod audio;

use soundovern_core::client::{ClientConfig, ConnState, Engine};
use std::sync::{Arc, Mutex};

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());
static LOG_INIT: std::sync::Once = std::sync::Once::new();

/// Route Rust `tracing`/`log` output and panics to logcat (tag `soundovern`).
fn init_logging() {
    LOG_INIT.call_once(|| {
        #[cfg(target_os = "android")]
        android_logger::init_once(android_logger::Config::default().with_tag("soundovern").with_max_level(log::LevelFilter::Debug));
        std::panic::set_hook(Box::new(|info| {
            log::error!("PANIC: {info}");
        }));
        tracing::info!("soundovern native {} loaded (protocol v{})", env!("CARGO_PKG_VERSION"), soundovern_proto::PROTO_VERSION);
    });
}

fn set_err(s: String) {
    *LAST_ERROR.lock().unwrap() = s;
}

/// # Safety
/// `h` must be a handle returned by `nativeStart` that has not been passed to `nativeStop`.
unsafe fn engine<'a>(h: jlong) -> Option<&'a Engine> {
    (h != 0).then(|| &*(h as *const Engine))
}

#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeStart(
    mut env: JNIEnv,
    _c: JClass,
    server: JString,
    pin: JString,
    name: JString,
    min_ms: jint,
    max_ms: jint,
) -> jlong {
    init_logging();
    let get = |env: &mut JNIEnv, s: &JString| -> String { env.get_string(s).map(|s| s.into()).unwrap_or_default() };
    let mut cfg = ClientConfig::new(get(&mut env, &server));
    cfg.pin = get(&mut env, &pin);
    cfg.client_name = get(&mut env, &name);
    cfg.min_ms = min_ms.max(10) as u32;
    cfg.max_ms = max_ms.max(min_ms + 10) as u32;
    tracing::info!("starting engine for {} (min {} ms, max {} ms)", cfg.server, cfg.min_ms, cfg.max_ms);
    match Engine::start(cfg) {
        Ok(e) => Arc::into_raw(e) as jlong,
        Err(msg) => {
            tracing::error!("engine start failed: {msg}");
            set_err(msg);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeLastError<'l>(env: JNIEnv<'l>, _c: JClass) -> JString<'l> {
    let s = LAST_ERROR.lock().unwrap().clone();
    env.new_string(s).expect("jstring")
}

/// Fills `buf[0..n]` with interleaved stereo float samples. Call from the audio thread.
#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeRender(env: JNIEnv, _c: JClass, h: jlong, buf: JFloatArray, n: jint) {
    let Some(e) = (unsafe { engine(h) }) else { return };
    let mut tmp = vec![0f32; n.max(0) as usize];
    e.render(&mut tmp);
    let _ = env.set_float_array_region(&buf, 0, &tmp);
}

#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeSetMuted(_env: JNIEnv, _c: JClass, h: jlong, m: jboolean) {
    if let Some(e) = unsafe { engine(h) } {
        e.set_muted(m != 0);
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeSetVolume(_env: JNIEnv, _c: JClass, h: jlong, v: jfloat) {
    if let Some(e) = unsafe { engine(h) } {
        e.set_volume(v);
    }
}

/// Returns [state(0 connected,1 reconnecting,2 stopped), received, lost, late, underruns, jitterMs, bufferMs].
#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeStats<'l>(env: JNIEnv<'l>, _c: JClass, h: jlong) -> JFloatArray<'l> {
    let out = env.new_float_array(7).expect("float array");
    if let Some(e) = unsafe { engine(h) } {
        let s = e.stats();
        let st = match s.state {
            ConnState::Connected => 0.0,
            ConnState::Reconnecting => 1.0,
            ConnState::Stopped => 2.0,
        };
        let v = [st, s.jitter.received as f32, s.jitter.lost as f32, s.jitter.late as f32, s.jitter.underruns as f32, s.jitter_ms, s.buffer_ms];
        let _ = env.set_float_array_region(&out, 0, &v);
    }
    out
}

/// Stops the engine and releases the handle. The handle must not be used afterwards.
#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeStop(_env: JNIEnv, _c: JClass, h: jlong) {
    if h != 0 {
        let e = unsafe { Arc::from_raw(h as *const Engine) };
        e.stop();
    }
}

/// Borrow the engine behind a handle as an `Arc` (bumping the refcount).
///
/// # Safety
/// `h` must be a live handle from `nativeStart`.
#[cfg(target_os = "android")]
unsafe fn engine_arc(h: jlong) -> Option<Arc<Engine>> {
    if h == 0 {
        return None;
    }
    let p = h as *const Engine;
    Arc::increment_strong_count(p);
    Some(Arc::from_raw(p))
}

/// Starts the Oboe output stream for an engine. Returns an audio handle, or 0 (see `nativeLastError`).
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeAudioStart(_env: JNIEnv, _c: JClass, h: jlong) -> jlong {
    let Some(e) = (unsafe { engine_arc(h) }) else { return 0 };
    match audio::AudioOut::start(e) {
        Ok(a) => Box::into_raw(Box::new(a)) as jlong,
        Err(m) => {
            set_err(m);
            0
        }
    }
}

/// Returns [outputLatencyMs, bufferFrames, burstFrames, xruns, exclusive(0/1), reopens].
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeAudioInfo<'l>(env: JNIEnv<'l>, _c: JClass, a: jlong) -> JFloatArray<'l> {
    use std::sync::atomic::Ordering::Relaxed;
    let out = env.new_float_array(6).expect("float array");
    if a != 0 {
        let a = unsafe { &*(a as *const audio::AudioOut) };
        let i = &a.info;
        let v = [i.latency_ms(), i.buffer_frames.load(Relaxed) as f32, i.burst_frames.load(Relaxed) as f32, i.xruns.load(Relaxed) as f32, i.exclusive.load(Relaxed) as u8 as f32, i.reopens.load(Relaxed) as f32];
        let _ = env.set_float_array_region(&out, 0, &v);
    }
    out
}

/// Stops and frees the audio output. Must be called before `nativeStop`.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_dev_soundovern_NativeEngine_nativeAudioStop(_env: JNIEnv, _c: JClass, a: jlong) {
    if a != 0 {
        unsafe { Box::from_raw(a as *mut audio::AudioOut) }.stop();
    }
}
