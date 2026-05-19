package sh.sandboxed.dashboard.data.api

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import sh.sandboxed.dashboard.data.AppSettings

sealed class DesktopStreamEvent {
    data object Connected : DesktopStreamEvent()
    data class Frame(val bitmap: Bitmap) : DesktopStreamEvent()
    data class Error(val message: String) : DesktopStreamEvent()
    data class Closed(val reason: String?) : DesktopStreamEvent()
}

class DesktopStreamSocket(
    private val client: OkHttpClient,
    private val provider: () -> AppSettings,
) {
    @Volatile private var ws: WebSocket? = null

    fun connect(display: String, fps: Int, quality: Int): Flow<DesktopStreamEvent> = callbackFlow {
        val s = provider()
        val baseUrl = s.baseUrl.trimEnd('/').ifBlank { error("Server URL not configured") }
        val wsBase = baseUrl.replaceFirst("https://", "wss://").replaceFirst("http://", "ws://")
        val url = "$wsBase/api/desktop/stream".toHttpUrl().newBuilder()
            .addQueryParameter("display", display)
            .addQueryParameter("fps", fps.coerceIn(1, 30).toString())
            .addQueryParameter("quality", quality.coerceIn(10, 100).toString())
            .build()
        val protocols = listOfNotNull(
            "sandboxed",
            s.jwtToken?.let { "jwt.$it" },
        ).joinToString(", ")
        val req = Request.Builder()
            .url(url)
            .apply { if (protocols.isNotEmpty()) header("Sec-WebSocket-Protocol", protocols) }
            .build()

        val socket = client.newWebSocket(req, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                this@DesktopStreamSocket.ws = webSocket
                trySend(DesktopStreamEvent.Connected)
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                trySend(DesktopStreamEvent.Error(text))
            }

            override fun onMessage(webSocket: WebSocket, bytes: okio.ByteString) {
                val data = bytes.toByteArray()
                val bitmap = BitmapFactory.decodeByteArray(data, 0, data.size)
                if (bitmap == null) {
                    trySend(DesktopStreamEvent.Error("Could not decode desktop frame"))
                } else {
                    trySend(DesktopStreamEvent.Frame(bitmap))
                }
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(code, reason)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                this@DesktopStreamSocket.ws = null
                trySend(DesktopStreamEvent.Closed(reason))
                close()
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                this@DesktopStreamSocket.ws = null
                trySend(DesktopStreamEvent.Error(t.message ?: "Desktop stream failed"))
                close(t)
            }
        })

        awaitClose {
            socket.close(1000, "client closing")
            this@DesktopStreamSocket.ws = null
        }
    }

    fun pause() {
        ws?.send("""{"t":"pause"}""")
    }

    fun resume() {
        ws?.send("""{"t":"resume"}""")
    }

    fun setFps(fps: Int) {
        ws?.send("""{"t":"fps","fps":${fps.coerceIn(1, 30)}}""")
    }

    fun setQuality(quality: Int) {
        ws?.send("""{"t":"quality","quality":${quality.coerceIn(10, 100)}}""")
    }

    fun click(x: Int, y: Int, double: Boolean = false) {
        ws?.send("""{"t":"click","x":$x,"y":$y,"button":"left","double":$double}""")
    }

    fun scroll(x: Int, y: Int, amount: Int) {
        ws?.send("""{"t":"scroll","x":$x,"y":$y,"amount":$amount}""")
    }

    fun typeText(text: String) {
        if (text.isBlank()) return
        ws?.send("""{"t":"type","text":${JSONObject.quote(text)}}""")
    }

    fun key(key: String) {
        if (key.isBlank()) return
        ws?.send("""{"t":"key","key":${JSONObject.quote(key)}}""")
    }
}
