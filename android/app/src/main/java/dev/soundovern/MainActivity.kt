package dev.soundovern

import android.Manifest
import android.content.Intent
import android.content.SharedPreferences
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

class MainActivity : ComponentActivity() {
    private var discovery: Discovery? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        if (Build.VERSION.SDK_INT >= 33) requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        val prefs = getSharedPreferences("sn", MODE_PRIVATE)
        setContent {
            SoundovernTheme {
                Surface(Modifier.fillMaxSize(), color = Palette.Bg) {
                    Screen(
                        prefs = prefs,
                        onConnect = { server, pin ->
                            startForegroundService(
                                Intent(this, StreamService::class.java).setAction(StreamService.ACTION_START)
                                    .putExtra("server", server).putExtra("pin", pin)
                            )
                        },
                        onDisconnect = { startService(Intent(this, StreamService::class.java).setAction(StreamService.ACTION_STOP)) },
                        onToggleMute = { startService(Intent(this, StreamService::class.java).setAction(StreamService.ACTION_MUTE)) },
                        startDiscovery = { cb -> Discovery(applicationContext, cb).also { it.start(); discovery = it } },
                    )
                }
            }
        }
    }

    override fun onDestroy() { discovery?.stop(); super.onDestroy() }
}

private val Serif = FontFamily.Serif

@Composable
private fun Screen(
    prefs: SharedPreferences,
    onConnect: (String, String) -> Unit,
    onDisconnect: () -> Unit,
    onToggleMute: () -> Unit,
    startDiscovery: ((List<Found>) -> Unit) -> Unit,
) {
    var server by remember { mutableStateOf(prefs.getString("server", "") ?: "") }
    var pin by remember { mutableStateOf(prefs.getString("pin", "") ?: "") }
    var found by remember { mutableStateOf(listOf<Found>()) }
    var stats by remember { mutableStateOf(StreamService.stats) }
    var running by remember { mutableStateOf(StreamService.running) }
    var muted by remember { mutableStateOf(StreamService.muted) }
    var volume by remember { mutableFloatStateOf(prefs.getFloat("volume", 1f)) }
    var showAbout by remember { mutableStateOf(!prefs.getBoolean("ai_notice_ack", false)) }
    var showLegal by remember { mutableStateOf(false) }
    val firstRun = !prefs.getBoolean("ai_notice_ack", false)

    LaunchedEffect(Unit) {
        startDiscovery { found = it }
        while (true) {
            stats = StreamService.stats; running = StreamService.running; muted = StreamService.muted
            delay(500)
        }
    }
    // Re-apply the saved volume whenever a session starts.
    LaunchedEffect(stats.phase) {
        if (stats.phase == Phase.PLAYING && StreamService.handle != 0L) NativeEngine.nativeSetVolume(StreamService.handle, volume)
    }

    if (showAbout) AiNotice(firstRun = firstRun, onLegal = { showLegal = true }) {
        prefs.edit().putBoolean("ai_notice_ack", true).apply()
        showAbout = false
    }
    if (showLegal) LegalScreen { showLegal = false }

    Column(
        Modifier.fillMaxSize().systemBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Column(Modifier.padding(top = 8.dp)) {
            Text("Soundovern", style = MaterialTheme.typography.headlineLarge, color = Palette.Text)
            Text("Your desktop's audio, on this phone.", style = MaterialTheme.typography.bodyMedium, color = Palette.Muted)
        }

        StatusPanel(stats)

        if (stats.phase == Phase.PLAYING) {
            Metrics(stats)
            if (!stats.exclusive && stats.outMs > 100f) {
                Note(
                    "Slow output path",
                    "Audio is leaving this phone through a route that adds about ${stats.outMs.toInt()} ms (Bluetooth or a shared audio path). " +
                        "The phone speaker or wired headphones are much faster.",
                )
            }
            Panel {
                Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    TextButton(onClick = onToggleMute) {
                        Text(if (muted) "Unmute" else "Mute", color = if (muted) Palette.Clay else Palette.Text)
                    }
                    Slider(
                        value = volume,
                        onValueChange = {
                            volume = it
                            if (StreamService.handle != 0L) NativeEngine.nativeSetVolume(StreamService.handle, it)
                        },
                        onValueChangeFinished = { prefs.edit().putFloat("volume", volume).apply() },
                        colors = SliderDefaults.colors(thumbColor = Palette.Clay, activeTrackColor = Palette.Clay, inactiveTrackColor = Palette.Outline),
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }

        if (!running) {
            Panel {
                Text("Connect", style = MaterialTheme.typography.titleMedium, color = Palette.Text)
                Field(server, { server = it }, "Desktop address", "192.168.1.20:7420", KeyboardType.Uri)
                Field(pin, { pin = it }, "Pairing PIN", "shown by the desktop server", KeyboardType.NumberPassword, hide = true)
            }
            if (found.isNotEmpty()) {
                Text("On your network", style = MaterialTheme.typography.labelMedium, color = Palette.Muted, modifier = Modifier.padding(start = 4.dp))
                found.forEach { f ->
                    Row(
                        Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(Palette.Surface)
                            .clickable { server = "${f.host}:${f.port}" }.padding(horizontal = 18.dp, vertical = 14.dp),
                        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(f.name, style = MaterialTheme.typography.titleMedium, color = Palette.Text)
                            Text("${f.host}:${f.port}", style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
                        }
                        Text("Use", style = MaterialTheme.typography.labelMedium, color = Palette.Clay)
                    }
                }
            }
        }

        val canConnect = server.isNotBlank() && pin.isNotBlank()
        if (running) {
            FilledTonalButton(
                onClick = onDisconnect,
                shape = RoundedCornerShape(16.dp),
                colors = ButtonDefaults.filledTonalButtonColors(containerColor = Palette.SurfaceHi, contentColor = Palette.Text),
                modifier = Modifier.fillMaxWidth().height(54.dp),
            ) { Text("Disconnect", style = MaterialTheme.typography.labelLarge) }
        } else {
            Button(
                onClick = {
                    prefs.edit().putString("server", server.trim()).putString("pin", pin.trim()).apply()
                    onConnect(server.trim(), pin.trim())
                },
                enabled = canConnect,
                shape = RoundedCornerShape(16.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = Palette.Clay, contentColor = Palette.Bg,
                    disabledContainerColor = Palette.SurfaceHi, disabledContentColor = Palette.Muted,
                ),
                modifier = Modifier.fillMaxWidth().height(54.dp),
            ) { Text("Connect", style = MaterialTheme.typography.labelLarge) }
        }

        Row(Modifier.fillMaxWidth().padding(bottom = 8.dp), horizontalArrangement = Arrangement.Center, verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
            TextButton(onClick = { showAbout = true }) {
                Text("Made with AI · About", style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
            }
        }
    }
}

@Composable
private fun Panel(content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(Palette.Surface).padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        content = content,
    )
}

@Composable
private fun StatusPanel(s: Stats) {
    val (label, color) = when (s.phase) {
        Phase.IDLE -> "Not connected" to Palette.Muted
        Phase.CONNECTING -> "Connecting…" to Palette.Warn
        Phase.PLAYING -> "Streaming" to Palette.Ok
        Phase.RECONNECTING -> "Reconnecting…" to Palette.Warn
        Phase.FAILED -> "Couldn't connect" to Palette.Err
    }
    Panel {
        Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Box(Modifier.size(10.dp).background(color, CircleShape))
            Text(label, style = MaterialTheme.typography.titleMedium, color = Palette.Text)
        }
        when (s.phase) {
            Phase.PLAYING -> {
                val total = s.jitterMs + s.outMs + 15f // + ~15 ms capture, encode and link
                Text(
                    if (s.outMs > 0f) "≈ ${total.toInt()} ms" else "—",
                    fontFamily = Serif, fontSize = 44.sp, color = Palette.Text,
                )
                Text("estimated delay from desktop to speaker", style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
            }
            Phase.FAILED -> Text(friendly(s.message), style = MaterialTheme.typography.bodyMedium, color = Palette.Muted)
            Phase.IDLE -> Text("Start the server on your desktop, then enter its address and PIN.", style = MaterialTheme.typography.bodyMedium, color = Palette.Muted)
            else -> {}
        }
    }
}

/** Turn engine errors into something a person can act on. */
private fun friendly(msg: String): String = when {
    "wrong PIN" in msg || "pairing failed" in msg -> "Pairing failed. Check the PIN shown on the desktop."
    "too many failed" in msg -> "The desktop is locking out new pairings after repeated wrong PINs. Wait a moment and try again."
    "Connection refused" in msg || "timed out" in msg -> "Can't reach the desktop. Is the server running, and are you on the same network?"
    "protocol version" in msg -> "This app and the desktop server are different versions. Update both."
    else -> msg
}

@Composable
private fun Metrics(s: Stats) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Tile("Buffer", "${s.jitterMs.toInt()} ms", "jitter absorber", Modifier.weight(1f))
            Tile("Output", "${s.outMs.toInt()} ms", if (s.exclusive) "exclusive · fast path" else "shared path", Modifier.weight(1f))
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Tile("Lost", "${s.lost}", "packets", Modifier.weight(1f), if (s.lost > 0) Palette.Warn else Palette.Text)
            Tile("Dropouts", "${s.underruns + s.xruns}", "${s.underruns} buffer · ${s.xruns} device", Modifier.weight(1f), if (s.underruns + s.xruns > 0) Palette.Warn else Palette.Text)
        }
    }
}

@Composable
private fun Tile(label: String, value: String, sub: String, modifier: Modifier, valueColor: Color = Palette.Text) {
    Column(modifier.clip(RoundedCornerShape(16.dp)).background(Palette.Surface).padding(14.dp)) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = Palette.Muted)
        Text(value, fontFamily = Serif, fontSize = 26.sp, color = valueColor)
        Text(sub, style = MaterialTheme.typography.bodySmall, color = Palette.Muted)
    }
}

@Composable
private fun Note(title: String, body: String) {
    Column(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(Palette.ClayDim.copy(alpha = 0.35f)).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(title, style = MaterialTheme.typography.titleMedium, color = Palette.Text)
        Text(body, style = MaterialTheme.typography.bodyMedium, color = Palette.Text.copy(alpha = 0.85f))
    }
}

@Composable
private fun Field(value: String, onChange: (String) -> Unit, label: String, hint: String, type: KeyboardType, hide: Boolean = false) {
    OutlinedTextField(
        value = value, onValueChange = onChange, singleLine = true,
        label = { Text(label) },
        placeholder = { Text(hint, color = Palette.Muted.copy(alpha = 0.6f)) },
        keyboardOptions = KeyboardOptions(keyboardType = type),
        visualTransformation = if (hide) PasswordVisualTransformation() else androidx.compose.ui.text.input.VisualTransformation.None,
        shape = RoundedCornerShape(14.dp),
        colors = OutlinedTextFieldDefaults.colors(
            focusedBorderColor = Palette.Clay, unfocusedBorderColor = Palette.Outline,
            focusedLabelColor = Palette.Clay, unfocusedLabelColor = Palette.Muted,
            cursorColor = Palette.Clay, focusedTextColor = Palette.Text, unfocusedTextColor = Palette.Text,
        ),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun AiNotice(firstRun: Boolean, onLegal: () -> Unit, onDone: () -> Unit) {
    AlertDialog(
        onDismissRequest = { if (!firstRun) onDone() }, // the first-run notice must be acknowledged
        containerColor = Palette.Surface,
        shape = RoundedCornerShape(24.dp),
        title = { Text("Made with AI", fontFamily = Serif, fontSize = 26.sp, color = Palette.Text) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(
                    "Soundovern was written with the help of Claude, an AI assistant made by Anthropic. " +
                        "A person directed and tested it, but the code hasn't had an independent security review.",
                    style = MaterialTheme.typography.bodyMedium, color = Palette.Text,
                )
                Text(
                    "Audio and control traffic are encrypted, and pairing needs the PIN your desktop shows. " +
                        "Even so, treat it like any young project: use it on networks you trust.",
                    style = MaterialTheme.typography.bodyMedium, color = Palette.Muted,
                )
            }
        },
        confirmButton = {
            TextButton(onClick = onDone) { Text(if (firstRun) "Got it" else "Close", color = Palette.Clay, style = MaterialTheme.typography.labelLarge) }
        },
        dismissButton = {
            TextButton(onClick = onLegal) { Text("Privacy & licenses", color = Palette.Muted, style = MaterialTheme.typography.labelLarge) }
        },
    )
}

private val LEGAL_TABS = listOf("Privacy" to "PRIVACY.txt", "License" to "LICENSE.txt", "Third-party" to "THIRD_PARTY_LICENSES.txt")

/** Full-screen viewer for the bundled privacy policy, license and third-party notices. */
@Composable
private fun LegalScreen(onClose: () -> Unit) {
    val context = LocalContext.current
    var tab by remember { mutableIntStateOf(0) }
    // The third-party file is large, so show it as paragraphs in a lazy list rather than one huge Text.
    val paragraphs by produceState(initialValue = listOf("Loading…"), tab) {
        value = try {
            context.assets.open(LEGAL_TABS[tab].second).bufferedReader().use { it.readText() }.split("\n\n").filter { it.isNotBlank() }
        } catch (e: Exception) {
            listOf("Couldn't load this document: ${e.message}")
        }
    }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(Modifier.fillMaxSize(), color = Palette.Bg) {
            Column(Modifier.systemBarsPadding()) {
                Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 8.dp), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                    Text("Privacy & licenses", fontFamily = Serif, fontSize = 22.sp, color = Palette.Text, modifier = Modifier.weight(1f).padding(start = 8.dp))
                    TextButton(onClick = onClose) { Text("Close", color = Palette.Clay) }
                }
                Row(Modifier.padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    LEGAL_TABS.forEachIndexed { i, (name, _) ->
                        val sel = i == tab
                        Text(
                            name, style = MaterialTheme.typography.labelLarge, color = if (sel) Palette.Bg else Palette.Muted,
                            modifier = Modifier.clip(RoundedCornerShape(50)).background(if (sel) Palette.Clay else Palette.Surface)
                                .clickable { tab = i }.padding(horizontal = 16.dp, vertical = 8.dp),
                        )
                    }
                }
                LazyColumn(Modifier.fillMaxSize().padding(horizontal = 20.dp), contentPadding = PaddingValues(vertical = 16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    items(paragraphs) { para ->
                        Text(para, style = MaterialTheme.typography.bodySmall, color = Palette.Text.copy(alpha = 0.9f), fontFamily = if (tab == 0) FontFamily.SansSerif else FontFamily.Monospace)
                    }
                }
            }
        }
    }
}
