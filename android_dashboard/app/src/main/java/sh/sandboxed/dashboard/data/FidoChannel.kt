package sh.sandboxed.dashboard.data

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive
import sh.sandboxed.dashboard.data.api.ApiService
import sh.sandboxed.dashboard.data.api.SseClient

/**
 * Single-source-of-truth listener for global SSE events.
 * - Captures `fido_sign_request` events and applies user-configured auto-approval rules:
 *   • If a non-expired rule matches and neither `requireBiometric` nor the global flag is on,
 *     auto-respond approved silently (no UI).
 *   • Otherwise, push to the pending queue so [FidoOverlay] can prompt — biometric is enforced
 *     by the overlay regardless.
 * - Forwards everything else via [events] (StateFlow, conflated — use the per-feature ViewModels'
 *   own SSE subscriptions for ordered event handling).
 */
class FidoChannel(
    private val sse: SseClient,
    private val api: ApiService,
    private val scope: CoroutineScope,
    private val cached: StateFlow<AppSettings>,
) {
    private val _pending = MutableStateFlow<List<FidoSignRequest>>(emptyList())
    val pending: StateFlow<List<FidoSignRequest>> = _pending.asStateFlow()

    private val _events = MutableStateFlow<SseEvent?>(null)
    val events: StateFlow<SseEvent?> = _events.asStateFlow()

    private val _connected = MutableStateFlow(false)
    val connected: StateFlow<Boolean> = _connected.asStateFlow()

    private var job: Job? = null

    fun start() {
        if (job?.isActive == true) return
        job = scope.launch {
            // Let the first Compose frame render before we open the SSE socket.
            delay(500)
            var attempt = 0
            while (true) {
                if (cached.value.jwtToken == null && cached.value.baseUrl.isBlank()) {
                    delay(1_000); continue
                }
                try {
                    sse.stream().collect { evt ->
                        attempt = 0
                        _connected.value = true
                        _events.value = evt
                        if (evt.type == "fido_sign_request") {
                            val req = parseFido(evt.data as? JsonObject) ?: return@collect
                            handleFido(req)
                        }
                    }
                } catch (_: Throwable) {}
                _connected.value = false
                attempt += 1
                delay((1000L shl minOf(attempt, 5)).coerceAtMost(30_000L))
            }
        }
    }

    fun resolve(requestId: String) {
        _pending.update { it.filterNot { r -> r.requestId == requestId } }
    }

    private suspend fun handleFido(req: FidoSignRequest) {
        val now = System.currentTimeMillis() / 1000
        val s = cached.value
        val rule = s.fidoRules.firstOrNull { it.matches(req) && !it.isExpired(now) }
        if (rule == null) {
            enqueue(req); return
        }
        if (rule.requireBiometric || s.fidoRequireBiometricAll) {
            // Still surface to user (overlay will require biometric on Approve).
            enqueue(req); return
        }
        // Silent auto-approve.
        runCatching { api.fidoRespond(req.requestId, true) }
    }

    private fun enqueue(req: FidoSignRequest) {
        _pending.update { current -> if (current.any { it.requestId == req.requestId }) current else current + req }
    }

    private fun parseFido(obj: JsonObject?): FidoSignRequest? {
        obj ?: return null
        val rid = obj["request_id"]?.jsonPrimitive?.content ?: return null
        return FidoSignRequest(
            requestId = rid,
            keyType = obj["key_type"]?.jsonPrimitive?.content.orEmpty(),
            keyFingerprint = obj["key_fingerprint"]?.jsonPrimitive?.content.orEmpty(),
            origin = obj["origin"]?.jsonPrimitive?.content.orEmpty(),
            hostname = obj["hostname"]?.jsonPrimitive?.content,
            workspace = obj["workspace"]?.jsonPrimitive?.content,
            expiresAt = obj["expires_at"]?.jsonPrimitive?.content.orEmpty(),
        )
    }
}
