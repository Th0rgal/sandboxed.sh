package sh.sandboxed.dashboard.data

import java.util.UUID

sealed class ChatMessageKind {
    data object User : ChatMessageKind()
    data class Assistant(
        val success: Boolean = true,
        val costCents: Int = 0,
        val costSource: String = "actual",
        val model: String? = null,
        val sharedFiles: List<SharedFile> = emptyList(),
    ) : ChatMessageKind()
    data class Thinking(val done: Boolean = false, val startTimeMs: Long = System.currentTimeMillis()) : ChatMessageKind()
    data class Phase(val phase: String, val detail: String? = null, val agent: String? = null) : ChatMessageKind()
    data class ToolCall(val name: String, val isActive: Boolean = true) : ChatMessageKind()
    data class ToolUi(val name: String, val content: ToolUiContent) : ChatMessageKind()
    data class Goal(val iteration: Int = 0, val status: String = "", val objective: String = "") : ChatMessageKind()
    data object SystemNote : ChatMessageKind()
    data object ErrorMsg : ChatMessageKind()
}

data class SharedFile(
    val name: String,
    val url: String,
    val contentType: String,
    val sizeBytes: Long? = null,
)

data class ChatMessage(
    val id: String = UUID.randomUUID().toString(),
    val kind: ChatMessageKind,
    val content: String,
    val timestamp: Long = System.currentTimeMillis(),
)
