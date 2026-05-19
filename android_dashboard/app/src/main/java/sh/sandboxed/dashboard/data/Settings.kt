package sh.sandboxed.dashboard.data

import android.content.Context
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

private val Context.dataStore by preferencesDataStore(name = "sandboxed_settings")

data class AppSettings(
    val baseUrl: String = "",
    val jwtToken: String? = null,
    val lastUsername: String = "",
    val defaultAgent: String = "",
    val defaultBackend: String = "",
    val skipAgentSelection: Boolean = false,
    val controlDraftText: String = "",
    val lastMissionId: String? = null,
    val fidoRules: List<AutoApprovalRule> = emptyList(),
    val fidoRequireBiometricAll: Boolean = false,
) {
    val isConfigured: Boolean get() = baseUrl.isNotBlank()
}

class SettingsStore(private val ctx: Context) {
    private object Keys {
        val BASE_URL = stringPreferencesKey("api_base_url")
        val JWT_TOKEN = stringPreferencesKey("jwt_token")
        val LAST_USERNAME = stringPreferencesKey("last_username")
        val DEFAULT_AGENT = stringPreferencesKey("default_agent")
        val DEFAULT_BACKEND = stringPreferencesKey("default_backend")
        val SKIP_AGENT = booleanPreferencesKey("skip_agent_selection")
        val DRAFT = stringPreferencesKey("control_draft_text")
        val LAST_MISSION = stringPreferencesKey("control_last_mission_id")
        val FIDO_RULES_JSON = stringPreferencesKey("fido_auto_approval_rules")
        val FIDO_REQUIRE_BIOMETRIC_ALL = booleanPreferencesKey("fido_require_biometric_all")
    }

    private val rulesJson = Json { ignoreUnknownKeys = true }
    private val rulesSerializer = ListSerializer(AutoApprovalRule.serializer())

    val flow: Flow<AppSettings> = ctx.dataStore.data.map { prefs ->
        AppSettings(
            baseUrl = prefs[Keys.BASE_URL].orEmpty(),
            jwtToken = prefs[Keys.JWT_TOKEN]?.takeIf { it.isNotBlank() },
            lastUsername = prefs[Keys.LAST_USERNAME].orEmpty(),
            defaultAgent = prefs[Keys.DEFAULT_AGENT].orEmpty(),
            defaultBackend = prefs[Keys.DEFAULT_BACKEND].orEmpty(),
            skipAgentSelection = prefs[Keys.SKIP_AGENT] ?: false,
            controlDraftText = prefs[Keys.DRAFT].orEmpty(),
            lastMissionId = prefs[Keys.LAST_MISSION],
            fidoRules = prefs[Keys.FIDO_RULES_JSON]
                ?.let { runCatching { rulesJson.decodeFromString(rulesSerializer, it) }.getOrNull() }
                ?: emptyList(),
            fidoRequireBiometricAll = prefs[Keys.FIDO_REQUIRE_BIOMETRIC_ALL] ?: false,
        )
    }

    suspend fun setBaseUrl(value: String) = ctx.dataStore.edit { it[Keys.BASE_URL] = value.trimEnd('/') }
    suspend fun setToken(value: String?) = ctx.dataStore.edit {
        if (value.isNullOrBlank()) it.remove(Keys.JWT_TOKEN) else it[Keys.JWT_TOKEN] = value
    }
    suspend fun setLastUsername(value: String) = ctx.dataStore.edit { it[Keys.LAST_USERNAME] = value }
    suspend fun setDefaultAgent(value: String) = ctx.dataStore.edit { it[Keys.DEFAULT_AGENT] = value }
    suspend fun setDefaultBackend(value: String) = ctx.dataStore.edit { it[Keys.DEFAULT_BACKEND] = value }
    suspend fun setSkipAgentSelection(value: Boolean) = ctx.dataStore.edit { it[Keys.SKIP_AGENT] = value }
    suspend fun setDraft(value: String) = ctx.dataStore.edit { it[Keys.DRAFT] = value }
    suspend fun setLastMission(value: String?) = ctx.dataStore.edit {
        if (value == null) it.remove(Keys.LAST_MISSION) else it[Keys.LAST_MISSION] = value
    }

    suspend fun setFidoRules(rules: List<AutoApprovalRule>) = ctx.dataStore.edit {
        it[Keys.FIDO_RULES_JSON] = rulesJson.encodeToString(rulesSerializer, rules)
    }

    suspend fun setFidoRequireBiometricAll(value: Boolean) = ctx.dataStore.edit {
        it[Keys.FIDO_REQUIRE_BIOMETRIC_ALL] = value
    }
}
