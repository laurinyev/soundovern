package dev.soundovern

/** Thin JNI wrapper over the shared Rust engine (crates/soundovern-android-ffi). */
object NativeEngine {
    init { System.loadLibrary("soundovern_android_ffi") }

    /** Returns a handle, or 0 on failure (see [nativeLastError]). */
    external fun nativeStart(server: String, pin: String, name: String, minMs: Int, maxMs: Int): Long
    external fun nativeLastError(): String
    /** Fills the first [n] floats of [buf] with interleaved 48 kHz stereo (unused when Oboe output is active). */
    external fun nativeRender(handle: Long, buf: FloatArray, n: Int)
    /** Opens the low-latency Oboe output fed directly by the engine. Returns an audio handle, or 0. */
    external fun nativeAudioStart(handle: Long): Long
    /** [outputLatencyMs, bufferFrames, burstFrames, xruns, exclusive(0/1), reopens] */
    external fun nativeAudioInfo(audio: Long): FloatArray
    /** Stops the output; must be called before [nativeStop]. */
    external fun nativeAudioStop(audio: Long)
    external fun nativeSetMuted(handle: Long, muted: Boolean)
    external fun nativeSetVolume(handle: Long, volume: Float)
    /** [state(0 connected,1 reconnecting,2 stopped), received, lost, late, underruns, jitterMs, bufferMs] */
    external fun nativeStats(handle: Long): FloatArray
    /** Releases the handle; the audio thread must already have stopped calling nativeRender. */
    external fun nativeStop(handle: Long)
}
