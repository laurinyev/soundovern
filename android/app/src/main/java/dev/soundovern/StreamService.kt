package dev.soundovern

import android.app.*
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.*
import android.net.wifi.WifiManager
import android.os.*
import androidx.core.app.NotificationCompat
import kotlin.concurrent.thread

/** Foreground service: owns the engine handle and the low-latency AudioTrack render loop. */
class StreamService : Service() {
    companion object {
        const val ACTION_START = "dev.soundovern.START"
        const val ACTION_STOP = "dev.soundovern.STOP"
        const val ACTION_MUTE = "dev.soundovern.MUTE"
        private const val CHANNEL = "stream"
        private const val NOTIF_ID = 1

        /** Observable by the UI. */
        @Volatile var status: String = "Idle"
        @Volatile var running: Boolean = false
        @Volatile var handle: Long = 0
    }

    private var audioThread: Thread? = null
    @Volatile private var stopping = false
    @Volatile private var muted = false
    private var focus: AudioFocusRequest? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(i: Intent?) = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> { shutdown(); return START_NOT_STICKY }
            ACTION_MUTE -> { setMuted(!muted); return START_NOT_STICKY }
            ACTION_START -> if (!running) begin(
                intent.getStringExtra("server") ?: "", intent.getStringExtra("pin") ?: ""
            )
        }
        return START_NOT_STICKY
    }

    private fun begin(server: String, pin: String) {
        running = true; stopping = false
        status = "Connecting…"
        createChannel()
        val n = notification("Connecting to $server")
        if (Build.VERSION.SDK_INT >= 29)
            startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        else startForeground(NOTIF_ID, n)

        val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "soundovern").apply { acquire() }
        wakeLock = (getSystemService(POWER_SERVICE) as PowerManager)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "soundovern:stream").apply { acquire() }
        requestFocus()

        audioThread = thread(name = "sn-audio", priority = Thread.MAX_PRIORITY) {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
            val h = NativeEngine.nativeStart(server, pin, Build.MODEL, 20, 120)
            if (h == 0L) {
                status = "Failed: ${NativeEngine.nativeLastError()}"
                running = false
                shutdown(); return@thread
            }
            handle = h
            val rate = 48000
            val chunkFrames = 240 // 5 ms: finer pulls keep the jitter buffer depth steadier
            val chunk = chunkFrames * 2
            val am = getSystemService(AUDIO_SERVICE) as AudioManager
            val burst = am.getProperty(AudioManager.PROPERTY_OUTPUT_FRAMES_PER_BUFFER)?.toIntOrNull()?.takeIf { it > 0 } ?: 192
            val minBuf = AudioTrack.getMinBufferSize(rate, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_FLOAT)
            val track = AudioTrack.Builder()
                .setAudioAttributes(AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
                .setAudioFormat(AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_FLOAT).setSampleRate(rate)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build())
                .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(minBuf)
                .build()
            // Shrink the device queue to 1 hardware burst, grow it only if we underrun.
            var bufFrames = track.setBufferSizeInFrames(burst).coerceAtLeast(burst)
            val maxFrames = track.bufferCapacityInFrames
            var lastUnderruns = 0
            track.play()
            val buf = FloatArray(chunk)
            var lastUi = 0L
            while (!stopping) {
                NativeEngine.nativeRender(h, buf, chunk)
                track.write(buf, 0, chunk, AudioTrack.WRITE_BLOCKING) // paces us to the device clock
                val u = track.underrunCount
                if (u > lastUnderruns && bufFrames < maxFrames) {
                    lastUnderruns = u
                    bufFrames = track.setBufferSizeInFrames(minOf(bufFrames + burst, maxFrames))
                }
                val now = SystemClock.elapsedRealtime()
                if (now - lastUi > 1000) { lastUi = now; updateStatus(h, bufFrames * 1000 / rate) }
            }
            track.stop(); track.release()
            NativeEngine.nativeStop(h) // safe: render loop has exited
            handle = 0
        }
    }

    private fun updateStatus(h: Long, outMs: Int) {
        val s = NativeEngine.nativeStats(h)
        status = when (s[0].toInt()) {
            0 -> "Playing · jitter buf %.0f ms · out %d ms · lost %.0f · underruns %.0f".format(s[6], outMs, s[2], s[4])
            1 -> "Reconnecting…"
            else -> "Stopped"
        }
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).notify(NOTIF_ID, notification(status))
    }

    private fun setMuted(m: Boolean) {
        muted = m
        val h = handle
        if (h != 0L) NativeEngine.nativeSetMuted(h, m)
    }

    private fun requestFocus() {
        val am = getSystemService(AUDIO_SERVICE) as AudioManager
        val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
            .setOnAudioFocusChangeListener { change ->
                when (change) {
                    AudioManager.AUDIOFOCUS_LOSS -> shutdown()
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK -> setMuted(true)
                    AudioManager.AUDIOFOCUS_GAIN -> setMuted(false)
                }
            }.build()
        am.requestAudioFocus(req)
        focus = req
    }

    private fun createChannel() {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Streaming", NotificationManager.IMPORTANCE_LOW))
    }

    private fun action(title: String, act: String) = NotificationCompat.Action.Builder(
        0, title,
        PendingIntent.getService(this, act.hashCode(), Intent(this, StreamService::class.java).setAction(act),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    ).build()

    private fun notification(text: String): Notification =
        NotificationCompat.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle("Soundovern")
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE))
            .addAction(action(if (muted) "Unmute" else "Mute", ACTION_MUTE))
            .addAction(action("Disconnect", ACTION_STOP))
            .build()

    private fun shutdown() {
        stopping = true
        val t = audioThread
        if (t != null && t != Thread.currentThread()) t.join(1000)
        focus?.let { (getSystemService(AUDIO_SERVICE) as AudioManager).abandonAudioFocusRequest(it) }
        wifiLock?.takeIf { it.isHeld }?.release()
        wakeLock?.takeIf { it.isHeld }?.release()
        running = false
        if (!status.startsWith("Failed")) status = "Idle"
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() { if (running) shutdown() }
}
