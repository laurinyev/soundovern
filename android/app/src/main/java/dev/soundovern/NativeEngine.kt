package dev.soundovern

/** Thin JNI wrapper over the shared Rust engine (crates/soundovern-android-ffi). */
object NativeEngine {
    init { System.loadLibrary("soundovern_android_ffi") }

    /** Returns a handle, or 0 on failure (see [nativeLastError]). */
    external fun nativeStart(server: String, pin: String, name: String, minMs: Int, maxMs: Int): Long
    external fun nativeLastError(): String
    /** Fills the first [n] floats of [buf] with interleaved 48 kHz stereo. */
    external fun nativeRender(handle: Long, buf: FloatArray, n: Int)
    external fun nativeSetMuted(handle: Long, muted: Boolean)
    external fun nativeSetVolume(handle: Long, volume: Float)
    /** [state(0 connected,1 reconnecting,2 stopped), received, lost, late, underruns, jitterMs, bufferMs] */
    external fun nativeStats(handle: Long): FloatArray
    /** Releases the handle; the audio thread must already have stopped calling nativeRender. */
    external fun nativeStop(handle: Long)
}
