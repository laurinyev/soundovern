package dev.soundovern

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo

data class Found(val name: String, val host: String, val port: Int)

/** mDNS browse for `_soundovern._tcp`. Falls back gracefully: users can always type IP:port. */
class Discovery(ctx: Context, private val onChange: (List<Found>) -> Unit) {
    private val nsd = ctx.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val found = linkedMapOf<String, Found>()
    private var listener: NsdManager.DiscoveryListener? = null

    fun start() {
        val l = object : NsdManager.DiscoveryListener {
            override fun onServiceFound(s: NsdServiceInfo) {
                @Suppress("DEPRECATION")
                nsd.resolveService(s, object : NsdManager.ResolveListener {
                    override fun onServiceResolved(r: NsdServiceInfo) {
                        val host = r.host?.hostAddress ?: return
                        synchronized(found) { found[r.serviceName] = Found(r.serviceName, host, r.port); onChange(found.values.toList()) }
                    }
                    override fun onResolveFailed(s: NsdServiceInfo, code: Int) {}
                })
            }
            override fun onServiceLost(s: NsdServiceInfo) {
                synchronized(found) { found.remove(s.serviceName); onChange(found.values.toList()) }
            }
            override fun onDiscoveryStarted(t: String) {}
            override fun onDiscoveryStopped(t: String) {}
            override fun onStartDiscoveryFailed(t: String, c: Int) {}
            override fun onStopDiscoveryFailed(t: String, c: Int) {}
        }
        listener = l
        nsd.discoverServices("_soundovern._tcp", NsdManager.PROTOCOL_DNS_SD, l)
    }

    fun stop() { listener?.let { runCatching { nsd.stopServiceDiscovery(it) } }; listener = null }
}
