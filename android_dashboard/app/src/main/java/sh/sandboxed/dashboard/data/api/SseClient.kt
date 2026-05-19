package sh.sandboxed.dashboard.data.api

import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import okhttp3.Request
import okhttp3.sse.EventSource
import okhttp3.sse.EventSourceListener
import okhttp3.sse.EventSources
import sh.sandboxed.dashboard.data.SseEvent
import kotlinx.serialization.json.Json

class SseClient(private val api: ApiService, private val streamingClient: okhttp3.OkHttpClient) {

    fun stream(): Flow<SseEvent> = callbackFlow {
        val url = api.urlOf("/api/control/stream")
        val req: Request = api.newRequestBuilder(url)
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .build()

        val factory = EventSources.createFactory(streamingClient)
        val source: EventSource = factory.newEventSource(req, object : EventSourceListener() {
            override fun onEvent(eventSource: EventSource, id: String?, type: String?, data: String) {
                val t = type ?: "message"
                val parsed = runCatching { Json.parseToJsonElement(data) }.getOrNull()
                if (parsed != null) {
                    trySend(SseEvent(t, parsed))
                }
            }

            override fun onFailure(eventSource: EventSource, t: Throwable?, response: okhttp3.Response?) {
                close(t ?: RuntimeException("SSE failed: ${response?.code}"))
            }

            override fun onClosed(eventSource: EventSource) { close() }
        })
        awaitClose { source.cancel() }
    }
}
