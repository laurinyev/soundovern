//! JNI bridge: Kotlin `NativeEngine` <-> `soundovern_core::client::Engine`.
//! Build for Android with `cargo ndk -t arm64-v8a -t armeabi-v7a -o android/app/src/main/jniLibs build --release -p soundovern-android-ffi`.

use jni::objects::{JClass, JFloatArray, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};
use jni::JNIEnv;
use soundovern_core::client::{ClientConfig, ConnState, Engine};
use std::sync::{Arc, Mutex};

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

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
    let get = |env: &mut JNIEnv, s: &JString| -> String { env.get_string(s).map(|s| s.into()).unwrap_or_default() };
    let mut cfg = ClientConfig::new(get(&mut env, &server));
    let pin = get(&mut env, &pin);
    cfg.pin = (!pin.is_empty()).then_some(pin);
    cfg.client_name = get(&mut env, &name);
    cfg.min_ms = min_ms.max(10) as u32;
    cfg.max_ms = max_ms.max(min_ms + 10) as u32;
    match Engine::start(cfg) {
        Ok(e) => Arc::into_raw(e) as jlong,
        Err(msg) => {
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
