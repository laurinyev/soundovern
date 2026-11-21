package dev.soundovern

import android.app.*
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.*
import android.net.wifi.WifiManager
import android.os.*
import android.util.Log
import androidx.core.app.NotificationCompat
import kotlin.concurrent.thread

enum class Phase { IDLE, CONNECTING, PLAYING, RECONNECTING, FAILED }

/** Snapshot the UI renders. */
data class Stats(
    val phase: Phase = Phase.IDLE,
    val message: String = "",
    val jitterMs: Float = 0f,
    val outMs: Float = 0f,
    val exclusive: Boolean = false,
    val lost: Int = 0,
    val underruns: Int = 0,
    val xruns: Int = 0,
)

/** Foreground service: owns the engine handle; audio is rendered by the native Oboe stream. */
class StreamService : Service() {
    companion object {
        const val ACTION_START = "dev.soundovern.START"
        const val ACTION_STOP = "dev.soundovern.STOP"
        const val ACTION_MUTE = "dev.soundovern.MUTE"
        private const val CHANNEL = "stream"
        private const val NOTIF_ID = 1

        /** Observable by the UI. */
        @Volatile var status: String = "Idle"
        @Volatile var stats: Stats = Stats()
        @Volatile var muted: Boolean = false
        @Volatile var running: Boolean = false
        @Volatile var handle: Long = 0
    }

    private var audioThread: Thread? = null
    @Volatile private var stopping = false
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
        running = true; stopping = false; muted = false
        status = "Connecting…"
        stats = Stats(Phase.CONNECTING)
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

        audioThread = thread(name = "sn-engine") {
            val h = NativeEngine.nativeStart(server, pin, Build.MODEL, 20, 120)
            if (h == 0L) {
                fail(NativeEngine.nativeLastError())
                shutdown(); return@thread
            }
            handle = h
            val audio = NativeEngine.nativeAudioStart(h)
            if (audio == 0L) {
                fail(NativeEngine.nativeLastError())
                NativeEngine.nativeStop(h); handle = 0
                shutdown(); return@thread
            }
            // The audio callback runs natively; this thread only reports status.
            while (!stopping) {
                updateStatus(h, audio)
                try { Thread.sleep(1000) } catch (_: InterruptedException) { break }
            }
            NativeEngine.nativeAudioStop(audio) // stop the callback before freeing the engine
            NativeEngine.nativeStop(h)
            handle = 0
        }
    }

    private fun fail(msg: String) {
        status = "Failed: $msg"
        stats = Stats(Phase.FAILED, message = msg)
    }

    private fun updateStatus(h: Long, audio: Long) {
        val s = NativeEngine.nativeStats(h)
        val a = NativeEngine.nativeAudioInfo(audio)
        status = when (s[0].toInt()) {
            0 -> "Playing · jitter buf %.0f ms · out %.0f ms%s · lost %.0f · underruns %.0f · xruns %.0f".format(
                s[6], a[0], if (a[4] > 0f) " (exclusive)" else "", s[2], s[4], a[3])
            1 -> "Reconnecting…"
            else -> "Stopped"
        }
        stats = Stats(
            phase = when (s[0].toInt()) { 0 -> Phase.PLAYING; 1 -> Phase.RECONNECTING; else -> Phase.IDLE },
            jitterMs = s[6], outMs = a[0], exclusive = a[4] > 0f,
            lost = s[2].toInt(), underruns = s[4].toInt(), xruns = a[3].toInt(),
        )
        (getSystemService(NOTIFICATION_SERVICE) as NotificationManager).notify(NOTIF_ID, notification(status))
        if (++statusTick % 5 == 0) Log.i("soundovern", "status: $status (burst ${a[2].toInt()} fr, buffer ${a[1].toInt()} fr, reopens ${a[5].toInt()})")
    }

    private var statusTick = 0

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
        if (t != null && t != Thread.currentThread()) { t.interrupt(); t.join(2000) }
        focus?.let { (getSystemService(AUDIO_SERVICE) as AudioManager).abandonAudioFocusRequest(it) }
        wifiLock?.takeIf { it.isHeld }?.release()
        wakeLock?.takeIf { it.isHeld }?.release()
        running = false
        if (stats.phase != Phase.FAILED) { status = "Idle"; stats = Stats() }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() { if (running) shutdown() }
}
