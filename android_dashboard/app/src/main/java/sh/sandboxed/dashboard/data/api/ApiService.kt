package sh.sandboxed.dashboard.data.api

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.serializer
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.MultipartBody
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import sh.sandboxed.dashboard.data.AppSettings
import sh.sandboxed.dashboard.data.Backend
import sh.sandboxed.dashboard.data.BackendAgent
import sh.sandboxed.dashboard.data.ControlMessageRequest
import sh.sandboxed.dashboard.data.ControlMessageResponse
import sh.sandboxed.dashboard.data.CreateMissionRequest
import sh.sandboxed.dashboard.data.FileEntry
import sh.sandboxed.dashboard.data.FsPathRequest
import sh.sandboxed.dashboard.data.FsRmRequest
import sh.sandboxed.dashboard.data.GenericOk
import sh.sandboxed.dashboard.data.HealthResponse
import sh.sandboxed.dashboard.data.LoginRequest
import sh.sandboxed.dashboard.data.LoginResponse
import sh.sandboxed.dashboard.data.Mission
import sh.sandboxed.dashboard.data.ParallelConfig
import sh.sandboxed.dashboard.data.ParallelMessageRequest
import sh.sandboxed.dashboard.data.QueuedMessage
import sh.sandboxed.dashboard.data.RunningMissionInfo
import sh.sandboxed.dashboard.data.StatusUpdate
import sh.sandboxed.dashboard.data.StoredEvent
import sh.sandboxed.dashboard.data.Workspace
import java.io.File
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

class ApiService(
    private val client: OkHttpClient,
    private val provider: () -> AppSettings,
) {
    private val json get() = Net.json
    private val jsonMedia = "application/json".toMediaType()

    private fun base(): String =
        provider().baseUrl.ifBlank { error("Server URL not configured") }.trimEnd('/')

    fun urlOf(path: String, query: Map<String, String?> = emptyMap()): String {
        val raw = if (path.startsWith("/")) "${base()}$path" else "${base()}/$path"
        val builder = raw.toHttpUrl().newBuilder()
        query.forEach { (k, v) -> if (v != null) builder.addQueryParameter(k, v) }
        return builder.build().toString()
    }

    fun newRequestBuilder(url: String, authenticated: Boolean = true): Request.Builder {
        val b = Request.Builder().url(url)
        if (authenticated) {
            val token = provider().jwtToken
            if (!token.isNullOrBlank()) b.header("Authorization", "Bearer $token")
        }
        return b
    }

    suspend fun execute(req: Request): Response = suspendCancellableCoroutine { cont ->
        val call = client.newCall(req)
        cont.invokeOnCancellation { call.cancel() }
        call.enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) = cont.resumeWithException(e)
            override fun onResponse(call: Call, response: Response) = cont.resume(response)
        })
    }

    private suspend fun executeText(req: Request): String {
        val resp = execute(req)
        resp.use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw HttpException(r.code, "HTTP ${r.code}: $text")
            return text
        }
    }

    private fun emptyJsonBody(): RequestBody = "{}".toRequestBody(jsonMedia)

    private suspend inline fun <reified T> getJson(path: String, query: Map<String, String?> = emptyMap(), authenticated: Boolean = true): T = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf(path, query), authenticated).get().build()
        json.decodeFromString(serializer<T>(), executeText(req))
    }

    private suspend inline fun <reified T> getList(path: String, query: Map<String, String?> = emptyMap()): List<T> = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf(path, query)).get().build()
        json.decodeFromString(ListSerializer(serializer<T>()), executeText(req))
    }

    private suspend inline fun <reified Req, reified Res> postJson(path: String, body: Req, authenticated: Boolean = true): Res = withContext(Dispatchers.IO) {
        val rb = json.encodeToString(body).toRequestBody(jsonMedia)
        val req = newRequestBuilder(urlOf(path), authenticated).post(rb).build()
        json.decodeFromString(serializer<Res>(), executeText(req))
    }

    private suspend inline fun <reified Res> postEmpty(path: String): Res = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf(path)).post(emptyJsonBody()).build()
        json.decodeFromString(serializer<Res>(), executeText(req))
    }

    suspend fun health(): HealthResponse = getJson("/api/health", authenticated = false)
    suspend fun login(req: LoginRequest): LoginResponse = postJson("/api/auth/login", req, authenticated = false)

    suspend fun listMissions(limit: Int = 100, offset: Int = 0): List<Mission> =
        getList("/api/control/missions", mapOf("limit" to limit.toString(), "offset" to offset.toString()))

    suspend fun getMission(id: String): Mission = getJson("/api/control/missions/$id")
    suspend fun childMissions(parentId: String): List<Mission> =
        listMissions(limit = 200).filter { it.parentMissionId == parentId }
    suspend fun currentMission(): Mission? = runCatching { getJson<Mission>("/api/control/missions/current") }.getOrNull()
    suspend fun createMission(req: CreateMissionRequest): Mission = postJson("/api/control/missions", req)
    suspend fun loadMission(id: String): Mission = postEmpty("/api/control/missions/$id/load")
    suspend fun setStatus(id: String, status: String): GenericOk = postJson("/api/control/missions/$id/status", StatusUpdate(status))
    suspend fun resumeMission(id: String): Mission = postEmpty("/api/control/missions/$id/resume")
    suspend fun cancelMission(id: String): GenericOk = postEmpty("/api/control/missions/$id/cancel")
    suspend fun deleteMission(id: String) = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf("/api/control/missions/$id")).delete().build()
        executeText(req)
    }
    suspend fun cleanupMissions(): Int = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf("/api/control/missions/cleanup")).post(emptyJsonBody()).build()
        val obj = json.parseToJsonElement(executeText(req)).jsonObject
        obj["deleted_count"]?.jsonPrimitive?.content?.toIntOrNull() ?: 0
    }

    suspend fun missionEvents(id: String, sinceSeq: Long? = null, limit: Int? = null, latest: Boolean? = null, types: String? = null): Pair<List<StoredEvent>, Long?> = withContext(Dispatchers.IO) {
        val q = buildMap<String, String?> {
            if (sinceSeq != null) put("since_seq", sinceSeq.toString())
            if (limit != null) put("limit", limit.toString())
            if (latest != null) put("latest", latest.toString())
            if (types != null) put("types", types)
        }
        val req = newRequestBuilder(urlOf("/api/control/missions/$id/events", q)).get().build()
        val resp = execute(req)
        resp.use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw HttpException(r.code, "HTTP ${r.code}: $text")
            val list: List<StoredEvent> = json.decodeFromString(ListSerializer(serializer()), text)
            val maxSeq = r.header("X-Max-Sequence")?.toLongOrNull()
            list to maxSeq
        }
    }

    suspend fun sendMessage(content: String): ControlMessageResponse = postJson("/api/control/message", ControlMessageRequest(content))
    suspend fun cancelControl(): GenericOk = postEmpty("/api/control/cancel")
    suspend fun getQueue(): List<QueuedMessage> = getList("/api/control/queue")
    suspend fun deleteQueueItem(id: String) = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf("/api/control/queue/$id")).delete().build()
        executeText(req)
    }
    suspend fun clearQueue() = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf("/api/control/queue")).delete().build()
        executeText(req)
    }

    suspend fun running(): List<RunningMissionInfo> = getList("/api/control/running")
    suspend fun parallelConfig(): ParallelConfig = getJson("/api/control/parallel/config")
    suspend fun parallelSend(missionId: String, content: String, model: String? = null): GenericOk =
        postJson("/api/control/missions/$missionId/parallel", ParallelMessageRequest(content, model))

    suspend fun listFiles(path: String): List<FileEntry> = getList("/api/fs/list", mapOf("path" to path))
    suspend fun mkdir(path: String): GenericOk = postJson("/api/fs/mkdir", FsPathRequest(path))
    suspend fun rm(path: String, recursive: Boolean): GenericOk = postJson("/api/fs/rm", FsRmRequest(path, recursive))

    fun downloadUrl(path: String): String = urlOf("/api/fs/download", mapOf("path" to path))

    suspend fun downloadToFile(path: String, target: File) = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(downloadUrl(path)).get().build()
        execute(req).use { r ->
            if (!r.isSuccessful) throw HttpException(r.code, "HTTP ${r.code}")
            val body = r.body ?: throw IOException("empty body")
            target.outputStream().use { out -> body.byteStream().copyTo(out) }
        }
    }

    suspend fun uploadFile(remoteDir: String, fileName: String, contentType: String?, bytes: ByteArray): String = withContext(Dispatchers.IO) {
        val mediaType = (contentType ?: "application/octet-stream").toMediaType()
        val body = MultipartBody.Builder().setType(MultipartBody.FORM)
            .addFormDataPart("file", fileName, bytes.toRequestBody(mediaType))
            .build()
        val req = newRequestBuilder(urlOf("/api/fs/upload", mapOf("path" to remoteDir))).post(body).build()
        val text = executeText(req)
        val obj = json.parseToJsonElement(text) as? JsonObject
        obj?.get("path")?.jsonPrimitive?.content ?: remoteDir
    }

    suspend fun listWorkspaces(): List<sh.sandboxed.dashboard.data.Workspace> = getList("/api/workspaces")
    suspend fun getWorkspace(id: String): sh.sandboxed.dashboard.data.Workspace = getJson("/api/workspaces/$id")
    suspend fun createWorkspace(req: sh.sandboxed.dashboard.data.CreateWorkspaceRequest): sh.sandboxed.dashboard.data.Workspace =
        postJson("/api/workspaces", req)

    suspend fun listBackends(): List<Backend> = getList("/api/backends")
    suspend fun listBackendAgents(backendId: String): List<BackendAgent> = getList("/api/backends/$backendId/agents")

    // ---- Search ----
    suspend fun searchMissions(q: String, limit: Int = 25): List<sh.sandboxed.dashboard.data.MissionSearchResult> =
        getList("/api/control/missions/search", mapOf("q" to q, "limit" to limit.toString()))

    suspend fun searchMoments(q: String, missionId: String? = null, limit: Int = 25): List<sh.sandboxed.dashboard.data.MissionMomentSearchResult> =
        getList("/api/control/missions/search/moments", buildMap {
            put("q", q); put("limit", limit.toString())
            missionId?.let { put("mission_id", it) }
        })

    // ---- Tasks / Runs ----
    suspend fun listTasks(): List<sh.sandboxed.dashboard.data.TaskState> = getList("/api/tasks")
    suspend fun listRuns(limit: Int = 50, offset: Int = 0): sh.sandboxed.dashboard.data.RunsResponse =
        getJson("/api/runs", mapOf("limit" to limit.toString(), "offset" to offset.toString()))

    // ---- Providers / Library ----
    suspend fun listProviders(includeAll: Boolean = false): sh.sandboxed.dashboard.data.ProvidersResponse =
        getJson("/api/providers", mapOf("include_all" to includeAll.toString()))

    suspend fun listBuiltinCommands(): sh.sandboxed.dashboard.data.BuiltinCommandsResponse =
        getJson("/api/library/builtin-commands")

    // ---- FIDO ----
    suspend fun fidoRespond(requestId: String, approved: Boolean): GenericOk =
        postJson("/api/fido/respond", sh.sandboxed.dashboard.data.FidoRespondRequest(requestId, approved))

    // ---- Automations ----
    suspend fun listAutomations(missionId: String): List<sh.sandboxed.dashboard.data.Automation> =
        getList("/api/control/missions/$missionId/automations")

    suspend fun createAutomation(missionId: String, req: sh.sandboxed.dashboard.data.CreateAutomationRequest): sh.sandboxed.dashboard.data.Automation =
        postJson("/api/control/missions/$missionId/automations", req)

    suspend fun updateAutomation(id: String, req: sh.sandboxed.dashboard.data.UpdateAutomationRequest): sh.sandboxed.dashboard.data.Automation = withContext(Dispatchers.IO) {
        val rb = json.encodeToString(req).toRequestBody(jsonMedia)
        val r = newRequestBuilder(urlOf("/api/control/automations/$id")).patch(rb).build()
        json.decodeFromString(sh.sandboxed.dashboard.data.Automation.serializer(), executeText(r))
    }

    suspend fun deleteAutomation(id: String) = withContext(Dispatchers.IO) {
        val req = newRequestBuilder(urlOf("/api/control/automations/$id")).delete().build()
        executeText(req)
    }
}
