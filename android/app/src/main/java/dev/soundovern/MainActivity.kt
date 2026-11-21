package dev.soundovern

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay

class MainActivity : ComponentActivity() {
    private var discovery: Discovery? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33) requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        val prefs = getSharedPreferences("sn", MODE_PRIVATE)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    var server by remember { mutableStateOf(prefs.getString("server", "") ?: "") }
                    var pin by remember { mutableStateOf(prefs.getString("pin", "") ?: "") }
                    var found by remember { mutableStateOf(listOf<Found>()) }
                    var status by remember { mutableStateOf(StreamService.status) }
                    var running by remember { mutableStateOf(StreamService.running) }
                    LaunchedEffect(Unit) {
                        discovery = Discovery(applicationContext) { found = it }.also { it.start() }
                        while (true) { status = StreamService.status; running = StreamService.running; delay(500) }
                    }
                    Column(Modifier.padding(16.dp).statusBarsPadding(), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                        Text("Soundovern", style = MaterialTheme.typography.headlineMedium)
                        OutlinedTextField(server, { server = it }, label = { Text("Server (IP or IP:port)") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                        OutlinedTextField(pin, { pin = it }, label = { Text("PIN (shown by the server)") }, keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(keyboardType = androidx.compose.ui.text.input.KeyboardType.NumberPassword), singleLine = true, modifier = Modifier.fillMaxWidth())
                        Button(onClick = {
                            if (running) startService(Intent(this@MainActivity, StreamService::class.java).setAction(StreamService.ACTION_STOP))
                            else {
                                prefs.edit().putString("server", server).putString("pin", pin).apply()
                                startForegroundService(Intent(this@MainActivity, StreamService::class.java)
                                    .setAction(StreamService.ACTION_START).putExtra("server", server).putExtra("pin", pin))
                            }
                        }, enabled = running || (server.isNotBlank() && pin.isNotBlank())) { Text(if (running) "Disconnect" else "Connect") }
                        Text(status)
                        if (found.isNotEmpty()) Text("Found on network", style = MaterialTheme.typography.titleMedium)
                        LazyColumn {
                            items(found) { f ->
                                TextButton(onClick = { server = "${f.host}:${f.port}" }) { Text("${f.name}  ${f.host}:${f.port}") }
                            }
                        }
                    }
                }
            }
        }
    }

    override fun onDestroy() { discovery?.stop(); super.onDestroy() }
}
