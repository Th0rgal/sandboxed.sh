package sh.sandboxed.dashboard.data

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

@Serializable
enum class MissionStatus {
    @SerialName("pending") PENDING,
    @SerialName("active") ACTIVE,
    @SerialName("completed") COMPLETED,
    @SerialName("failed") FAILED,
    @SerialName("interrupted") INTERRUPTED,
    @SerialName("blocked") BLOCKED,
    @SerialName("not_feasible") NOT_FEASIBLE,
    @SerialName("unknown") UNKNOWN;

    val canResume: Boolean get() = this == INTERRUPTED || this == BLOCKED
}

@Serializable
data class MissionHistoryEntry(
    val role: String,
    val content: String,
)

@Serializable
data class Mission(
    val id: String,
    val status: MissionStatus = MissionStatus.UNKNOWN,
    val title: String? = null,
    @SerialName("short_description") val shortDescription: String? = null,
    @SerialName("metadata_updated_at") val metadataUpdatedAt: String? = null,
    @SerialName("metadata_source") val metadataSource: String? = null,
    @SerialName("metadata_model") val metadataModel: String? = null,
    @SerialName("metadata_version") val metadataVersion: String? = null,
    @SerialName("workspace_id") val workspaceId: String? = null,
    @SerialName("workspace_name") val workspaceName: String? = null,
    val agent: String? = null,
    @SerialName("model_override") val modelOverride: String? = null,
    val backend: String? = null,
    val history: List<MissionHistoryEntry> = emptyList(),
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("updated_at") val updatedAt: String = "",
    @SerialName("interrupted_at") val interruptedAt: String? = null,
    val resumable: Boolean = false,
    @SerialName("parent_mission_id") val parentMissionId: String? = null,
)

@Serializable
data class FileEntry(
    val name: String,
    val path: String,
    val kind: String,
    val size: Long = 0,
    val mtime: Long = 0,
) {
    val isDirectory: Boolean get() = kind == "dir"
    val isFile: Boolean get() = kind == "file"
}

@Serializable
data class Workspace(
    val id: String,
    val name: String,
    @SerialName("workspace_type") val workspaceType: String = "host",
    val path: String = "",
    val status: String = "ready",
    @SerialName("error_message") val errorMessage: String? = null,
    @SerialName("created_at") val createdAt: String = "",
    val skills: List<String> = emptyList(),
    val tools: List<String> = emptyList(),
    val plugins: List<String> = emptyList(),
    val template: String? = null,
    val distro: String? = null,
) {
    val isDefault: Boolean get() = id == "00000000-0000-0000-0000-000000000000"
}

@Serializable
data class CreateWorkspaceRequest(
    val name: String,
    @SerialName("workspace_type") val workspaceType: String = "container",
    val path: String? = null,
)

@Serializable
data class Backend(val id: String, val name: String)

@Serializable
data class BackendAgent(val id: String, val name: String)

@Serializable
data class HealthResponse(
    val status: String = "ok",
    @SerialName("auth_required") val authRequired: Boolean = false,
    @SerialName("auth_mode") val authMode: String? = null,
    @SerialName("github_enabled") val githubEnabled: Boolean = false,
)

@Serializable
data class LoginRequest(val password: String, val username: String? = null)

@Serializable
data class LoginResponse(val token: String, val exp: Long = 0)

@Serializable
data class CreateMissionRequest(
    @SerialName("workspace_id") val workspaceId: String? = null,
    val title: String? = null,
    val agent: String? = null,
    @SerialName("model_override") val modelOverride: String? = null,
    val backend: String? = null,
)

@Serializable
data class StatusUpdate(val status: String)

@Serializable
data class ControlMessageRequest(val content: String)

@Serializable
data class ControlMessageResponse(val id: String, val queued: Boolean = false)

@Serializable
data class ParallelMessageRequest(val content: String, val model: String? = null)

@Serializable
data class QueuedMessage(val id: String, val content: String, val agent: String? = null) {
    val displayContent: String get() = if (content.length <= 100) content else content.take(97) + "…"
}

@Serializable
data class RunningMissionInfo(
    @SerialName("mission_id") val missionId: String,
    val state: String = "",
    @SerialName("queue_len") val queueLen: Int = 0,
    @SerialName("history_len") val historyLen: Int = 0,
    @SerialName("seconds_since_activity") val secondsSinceActivity: Int = 0,
    @SerialName("expected_deliverables") val expectedDeliverables: Int = 0,
    @SerialName("current_activity") val currentActivity: String? = null,
    val title: String? = null,
) {
    val isRunning: Boolean get() = state == "running" || state == "waiting_for_tool"
    val isStalled: Boolean get() = isRunning && secondsSinceActivity > 60
    val isSeverelyStalled: Boolean get() = isRunning && secondsSinceActivity > 180
}

@Serializable
data class ParallelConfig(
    @SerialName("max_parallel_missions") val maxParallel: Int = 1,
    @SerialName("running_count") val runningCount: Int = 0,
)

@Serializable
data class StoredEvent(
    val id: Long = 0,
    @SerialName("mission_id") val missionId: String = "",
    val sequence: Long = 0,
    @SerialName("event_type") val eventType: String = "",
    val timestamp: String = "",
    @SerialName("event_id") val eventId: String? = null,
    @SerialName("tool_call_id") val toolCallId: String? = null,
    @SerialName("tool_name") val toolName: String? = null,
    val content: String = "",
    val metadata: JsonObject = JsonObject(emptyMap()),
)

@Serializable
data class FsPathRequest(val path: String)

@Serializable
data class FsRmRequest(val path: String, val recursive: Boolean = false)

@Serializable
data class GenericOk(val ok: Boolean = true)

data class SseEvent(val type: String, val data: JsonElement)

// ---- Search ----

@Serializable
data class MissionSearchResult(
    val mission: Mission,
    @SerialName("relevance_score") val relevanceScore: Double = 0.0,
)

@Serializable
data class MissionMomentSearchResult(
    val mission: Mission,
    @SerialName("entry_index") val entryIndex: Int = 0,
    val role: String = "",
    val snippet: String = "",
    val rationale: String = "",
    @SerialName("relevance_score") val relevanceScore: Double = 0.0,
)

// ---- Tasks & Runs ----

@Serializable
enum class TaskStatus {
    @SerialName("pending") PENDING,
    @SerialName("running") RUNNING,
    @SerialName("completed") COMPLETED,
    @SerialName("failed") FAILED,
    @SerialName("cancelled") CANCELLED,
}

@Serializable
data class TaskState(
    val id: String,
    val status: TaskStatus = TaskStatus.PENDING,
    val task: String = "",
    val model: String = "",
    val iterations: Int = 0,
    val result: String? = null,
)

@Serializable
data class Run(
    val id: String,
    @SerialName("created_at") val createdAt: String = "",
    val status: String = "",
    @SerialName("input_text") val inputText: String = "",
    @SerialName("final_output") val finalOutput: String? = null,
    @SerialName("total_cost_cents") val totalCostCents: Int = 0,
    @SerialName("summary_text") val summaryText: String? = null,
) {
    val costDollars: Double get() = totalCostCents / 100.0
}

@Serializable
data class RunsResponse(val runs: List<Run> = emptyList())

// ---- Providers / Slash Commands ----

@Serializable
data class Provider(
    val id: String,
    val name: String,
    val billing: String = "subscription",
    val description: String = "",
    val models: List<ProviderModel> = emptyList(),
)

@Serializable
data class ProviderModel(
    val id: String,
    val name: String,
    val description: String? = null,
)

@Serializable
data class ProvidersResponse(val providers: List<Provider> = emptyList())

@Serializable
data class SlashCommandParam(
    val name: String,
    val required: Boolean = false,
    val description: String? = null,
)

@Serializable
data class SlashCommand(
    val name: String,
    val description: String? = null,
    val path: String = "",
    val params: List<SlashCommandParam> = emptyList(),
)

@Serializable
data class BuiltinCommandsResponse(
    val opencode: List<SlashCommand> = emptyList(),
    val claudecode: List<SlashCommand> = emptyList(),
    val codex: List<SlashCommand> = emptyList(),
)

// ---- FIDO ----

@Serializable
data class FidoSignRequest(
    @SerialName("request_id") val requestId: String,
    @SerialName("key_type") val keyType: String = "",
    @SerialName("key_fingerprint") val keyFingerprint: String = "",
    val origin: String = "",
    val hostname: String? = null,
    val workspace: String? = null,
    @SerialName("expires_at") val expiresAt: String = "",
)

@Serializable
data class FidoRespondRequest(
    @SerialName("request_id") val requestId: String,
    val approved: Boolean,
)

// ---- Automations ----

@Serializable
data class Automation(
    val id: String,
    @SerialName("mission_id") val missionId: String,
    @SerialName("command_source") val commandSource: AutomationCommandSource,
    val trigger: AutomationTrigger,
    val variables: Map<String, String> = emptyMap(),
    val active: Boolean = true,
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("last_triggered_at") val lastTriggeredAt: String? = null,
    @SerialName("stop_policy") val stopPolicy: AutomationStopPolicy? = null,
    @SerialName("fresh_session") val freshSession: String? = null,
    @SerialName("retry_config") val retryConfig: AutomationRetryConfig? = null,
)

@Serializable
data class AutomationCommandSource(
    @SerialName("type")
    val kind: String = "inline",
    val content: String? = null,
    val name: String? = null,
    val path: String? = null,
)

@Serializable
data class AutomationTrigger(
    @SerialName("type")
    val kind: String = "interval",
    val seconds: Int? = null,
)

@Serializable
data class AutomationStopPolicy(
    @SerialName("type")
    val kind: String = "never",
    val count: Int? = null,
    val repo: String? = null,
)

@Serializable
data class AutomationRetryConfig(
    @SerialName("max_retries") val maxRetries: Int = 0,
    @SerialName("retry_delay_seconds") val retryDelaySeconds: Int = 0,
    @SerialName("backoff_multiplier") val backoffMultiplier: Double = 1.0,
)

@Serializable
data class CreateAutomationRequest(
    @SerialName("command_source") val commandSource: AutomationCommandSource,
    val trigger: AutomationTrigger,
    val variables: Map<String, String> = emptyMap(),
    val active: Boolean = true,
    @SerialName("stop_policy") val stopPolicy: AutomationStopPolicy? = null,
    @SerialName("fresh_session") val freshSession: String? = null,
    @SerialName("retry_config") val retryConfig: AutomationRetryConfig? = null,
)

@Serializable
data class UpdateAutomationRequest(
    @SerialName("command_source") val commandSource: AutomationCommandSource? = null,
    val trigger: AutomationTrigger? = null,
    val variables: Map<String, String>? = null,
    val active: Boolean? = null,
    @SerialName("stop_policy") val stopPolicy: AutomationStopPolicy? = null,
    @SerialName("fresh_session") val freshSession: String? = null,
    @SerialName("retry_config") val retryConfig: AutomationRetryConfig? = null,
)

// ---- Tool UI widgets ----

sealed class ToolUiContent {
    data class DataTable(val title: String?, val columns: List<TableColumn>, val rows: List<Map<String, String>>) : ToolUiContent()
    data class OptionList(val options: List<UiOption>, val multiSelect: Boolean) : ToolUiContent()
    data class Progress(val title: String?, val current: Int, val total: Int, val status: String?) : ToolUiContent() {
        val percentage: Float get() = if (total > 0) current.toFloat() / total else 0f
    }
    data class Alert(val title: String, val message: String?, val severity: String) : ToolUiContent()
    data class CodeBlock(val title: String?, val language: String?, val code: String, val lineNumbers: Boolean) : ToolUiContent()
    data class Unknown(val name: String, val rawArgs: String) : ToolUiContent()
}

data class TableColumn(val id: String, val label: String, val width: Int? = null)
data class UiOption(val id: String, val label: String, val description: String? = null, val disabled: Boolean = false)

object ToolUiParser {
    fun parse(name: String, args: kotlinx.serialization.json.JsonElement?): ToolUiContent {
        val obj = (args as? JsonObject) ?: return ToolUiContent.Unknown(name, args?.toString() ?: "")
        return when (name) {
            "ui_dataTable", "ui_data_table" -> ToolUiContent.DataTable(
                title = obj["title"]?.let { textOf(it) },
                columns = (obj["columns"] as? kotlinx.serialization.json.JsonArray).orEmpty().map {
                    val c = it as? JsonObject
                    if (c == null) {
                        val id = textOf(it) ?: ""
                        TableColumn(id = id, label = id)
                    } else {
                        TableColumn(
                            id = textOf(c["id"]) ?: "",
                            label = textOf(c["label"]) ?: textOf(c["id"]) ?: "",
                        )
                    }
                },
                rows = (obj["rows"] as? kotlinx.serialization.json.JsonArray).orEmpty().mapNotNull {
                    (it as? JsonObject)?.mapValues { (_, v) -> textOf(v) ?: v.toString() }
                },
            )
            "ui_optionList", "ui_option_list" -> ToolUiContent.OptionList(
                options = (obj["options"] as? kotlinx.serialization.json.JsonArray).orEmpty().mapNotNull {
                    val o = (it as? JsonObject) ?: return@mapNotNull null
                    UiOption(
                        id = textOf(o["id"]) ?: "",
                        label = textOf(o["label"]) ?: "",
                        description = textOf(o["description"]),
                        disabled = textOf(o["disabled"]) == "true",
                    )
                },
                multiSelect = textOf(obj["selectionMode"]) == "multi" || textOf(obj["multiple"]) == "true",
            )
            "ui_progress" -> ToolUiContent.Progress(
                title = textOf(obj["title"]),
                current = textOf(obj["current"])?.toIntOrNull() ?: 0,
                total = textOf(obj["total"])?.toIntOrNull() ?: 0,
                status = textOf(obj["status"]),
            )
            "ui_alert", "ui_notification" -> ToolUiContent.Alert(
                title = textOf(obj["title"]) ?: "",
                message = textOf(obj["message"]),
                severity = textOf(obj["type"]) ?: "info",
            )
            "ui_codeBlock", "ui_code", "ui_code_block" -> ToolUiContent.CodeBlock(
                title = textOf(obj["title"]),
                language = textOf(obj["language"]),
                code = textOf(obj["code"]) ?: "",
                lineNumbers = textOf(obj["lineNumbers"]) == "true",
            )
            else -> ToolUiContent.Unknown(name, obj.toString())
        }
    }

    private fun textOf(e: kotlinx.serialization.json.JsonElement?): String? {
        if (e == null) return null
        return runCatching { (e as? kotlinx.serialization.json.JsonPrimitive)?.content }.getOrNull()
    }
}

// ---- Auto-approval rules (FIDO) — local persistence model ----

@Serializable
enum class AutoApprovalRuleType {
    @SerialName("all_ssh") ALL_SSH,
    @SerialName("hostname") HOSTNAME,
    @SerialName("key_fingerprint") KEY_FINGERPRINT,
}

@Serializable
data class AutoApprovalRule(
    val id: String,
    val ruleType: AutoApprovalRuleType,
    val value: String? = null,
    val expiresAtEpochSec: Long? = null,
    val requireBiometric: Boolean = false,
    val createdAtEpochSec: Long = 0,
) {
    fun isExpired(nowEpochSec: Long): Boolean = expiresAtEpochSec != null && nowEpochSec > expiresAtEpochSec

    fun matches(req: FidoSignRequest): Boolean = when (ruleType) {
        AutoApprovalRuleType.ALL_SSH -> req.keyType.contains("ssh", ignoreCase = true)
        AutoApprovalRuleType.HOSTNAME -> req.hostname != null && req.hostname == value
        AutoApprovalRuleType.KEY_FINGERPRINT -> req.keyFingerprint == value
    }
}
