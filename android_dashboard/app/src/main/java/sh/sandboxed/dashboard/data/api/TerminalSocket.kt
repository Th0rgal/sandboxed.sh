package sh.sandboxed.dashboard.data.api

import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import sh.sandboxed.dashboard.data.AppSettings

sealed class TerminalEvent {
    data object Connected : TerminalEvent()
    data class Output(val text: String) : TerminalEvent()
    data class Closed(val reason: String?) : TerminalEvent()
    data class Failure(val error: Throwable) : TerminalEvent()
}

@Serializable
private data class WsInput(@SerialName("t") val t: String = "i", @SerialName("d") val d: String)

@Serializable
private data class WsResize(@SerialName("t") val t: String = "r", @SerialName("c") val c: Int, @SerialName("r") val r: Int)

class TerminalSocket(
    private val client: OkHttpClient,
    private val provider: () -> AppSettings,
) {
    @Volatile private var ws: WebSocket? = null

    fun connect(workspaceId: String? = null): Flow<TerminalEvent> = callbackFlow {
        val s = provider()
        val httpUrl = s.baseUrl.trimEnd('/').ifBlank { error("Server URL not configured") }
        val wsUrl = httpUrl.replaceFirst("https://", "wss://").replaceFirst("http://", "ws://")
        val path = if (workspaceId.isNullOrBlank()) "/api/console/ws" else "/api/workspaces/$workspaceId/shell"
        val protocols = listOfNotNull(
            "sandboxed",
            s.jwtToken?.let { "jwt.$it" }
        ).joinToString(", ")
        val req = Request.Builder()
            .url("$wsUrl$path")
            .apply { if (protocols.isNotEmpty()) header("Sec-WebSocket-Protocol", protocols) }
            .build()

        val socket = client.newWebSocket(req, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                this@TerminalSocket.ws = webSocket
                trySend(TerminalEvent.Connected)
                webSocket.send(Json.encodeToString(WsResize.serializer(), WsResize(c = 80, r = 24)))
            }
            override fun onMessage(webSocket: WebSocket, text: String) { trySend(TerminalEvent.Output(text)) }
            override fun onMessage(webSocket: WebSocket, bytes: okio.ByteString) {
                trySend(TerminalEvent.Output(bytes.utf8()))
            }
            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { webSocket.close(code, reason) }
            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                this@TerminalSocket.ws = null
                trySend(TerminalEvent.Closed(reason)); close()
            }
            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                this@TerminalSocket.ws = null
                trySend(TerminalEvent.Failure(t)); close(t)
            }
        })

        awaitClose {
            socket.close(1000, "client closing")
            this@TerminalSocket.ws = null
        }
    }

    fun sendInput(text: String) {
        ws?.send(Json.encodeToString(WsInput.serializer(), WsInput(d = text)))
    }

    fun sendResize(cols: Int, rows: Int) {
        ws?.send(Json.encodeToString(WsResize.serializer(), WsResize(c = cols, r = rows)))
    }
}
