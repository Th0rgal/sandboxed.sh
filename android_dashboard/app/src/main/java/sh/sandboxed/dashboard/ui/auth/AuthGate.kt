package sh.sandboxed.dashboard.ui.auth

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Code
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedButton
import androidx.compose.ui.platform.LocalContext
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.AppSettings
import sh.sandboxed.dashboard.data.LoginRequest
import sh.sandboxed.dashboard.ui.components.ErrorBanner
import sh.sandboxed.dashboard.ui.theme.Palette
import sh.sandboxed.dashboard.util.GitHubAuth

@Composable
fun AuthGate(
    container: AppContainer,
    settings: AppSettings,
    content: @Composable () -> Unit,
) {
    var phase by remember { mutableStateOf(AuthPhase.RESOLVING) }
    var authMode by remember { mutableStateOf<String?>(null) }
    var githubEnabled by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(settings.baseUrl, settings.jwtToken) {
        if (!settings.isConfigured) {
            phase = AuthPhase.NEEDS_CONFIG
            return@LaunchedEffect
        }
        phase = AuthPhase.RESOLVING
        try {
            val health = container.api.health()
            githubEnabled = health.githubEnabled
            if (!health.authRequired || health.authMode == "disabled") {
                phase = AuthPhase.AUTHENTICATED
            } else {
                authMode = health.authMode
                phase = if (settings.jwtToken != null) AuthPhase.AUTHENTICATED else AuthPhase.NEEDS_LOGIN
            }
        } catch (t: Throwable) {
            error = t.message
            phase = AuthPhase.NEEDS_CONFIG
        }
    }

    when (phase) {
        AuthPhase.RESOLVING -> FullscreenSpinner()
        AuthPhase.NEEDS_CONFIG -> ConfigSheet(container, settings, error)
        AuthPhase.NEEDS_LOGIN -> LoginScreen(container, settings, authMode ?: "single_tenant", githubEnabled)
        AuthPhase.AUTHENTICATED -> content()
    }
}

private enum class AuthPhase { RESOLVING, NEEDS_CONFIG, NEEDS_LOGIN, AUTHENTICATED }

@Composable
private fun FullscreenSpinner() {
    Box(
        Modifier.fillMaxSize().background(Palette.BackgroundPrimary),
        contentAlignment = Alignment.Center,
    ) { CircularProgressIndicator(color = Palette.Accent) }
}

@Composable
private fun ConfigSheet(container: AppContainer, settings: AppSettings, error: String?) {
    val scope = rememberCoroutineScope()
    var url by remember { mutableStateOf(settings.baseUrl.ifBlank { "https://" }) }
    var saving by remember { mutableStateOf(false) }

    Box(Modifier.fillMaxSize().background(Palette.BackgroundPrimary).padding(24.dp)) {
        Column(
            Modifier.align(Alignment.Center).widthIn(max = 480.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Sandboxed", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary)
            Text("Server URL", style = MaterialTheme.typography.titleMedium, color = Palette.TextSecondary)
            OutlinedTextField(
                value = url,
                onValueChange = { url = it },
                placeholder = { Text("https://sandboxed.example.com") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
                colors = TextFieldDefaults.colors(
                    focusedContainerColor = Palette.Card,
                    unfocusedContainerColor = Palette.Card,
                    focusedTextColor = Palette.TextPrimary,
                    unfocusedTextColor = Palette.TextPrimary,
                    cursorColor = Palette.Accent,
                ),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            )
            error?.let { ErrorBanner(it) }
            Button(
                onClick = {
                    saving = true
                    scope.launch {
                        runCatching { container.settings.setBaseUrl(url.trim()) }
                        saving = false
                    }
                },
                enabled = url.isNotBlank() && !saving,
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent, contentColor = Palette.TextPrimary),
            ) { Text(if (saving) "Saving…" else "Continue") }
        }
    }
}

@Composable
private fun LoginScreen(container: AppContainer, settings: AppSettings, authMode: String, githubEnabled: Boolean) {
    val scope = rememberCoroutineScope()
    val ctx = LocalContext.current
    var username by remember { mutableStateOf(settings.lastUsername) }
    var password by remember { mutableStateOf("") }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val multiUser = authMode == "multi_user"

    Box(Modifier.fillMaxSize().background(Palette.BackgroundPrimary).padding(24.dp)) {
        Column(
            Modifier.align(Alignment.Center).widthIn(max = 480.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Sign in", style = MaterialTheme.typography.headlineSmall, color = Palette.TextPrimary)
            Text(settings.baseUrl, style = MaterialTheme.typography.bodySmall, color = Palette.TextTertiary)
            Spacer(Modifier.height(4.dp))
            if (multiUser) {
                OutlinedTextField(
                    value = username,
                    onValueChange = { username = it },
                    label = { Text("Username") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                    colors = loginFieldColors(),
                )
            }
            OutlinedTextField(
                value = password,
                onValueChange = { password = it },
                label = { Text("Password") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                modifier = Modifier.fillMaxWidth(),
                colors = loginFieldColors(),
            )
            error?.let { ErrorBanner(it) }
            Button(
                onClick = {
                    loading = true; error = null
                    scope.launch {
                        runCatching {
                            val resp = container.api.login(LoginRequest(password = password, username = if (multiUser) username else null))
                            container.settings.setToken(resp.token)
                            if (multiUser) container.settings.setLastUsername(username)
                        }.onFailure { error = it.message }
                        loading = false
                    }
                },
                enabled = password.isNotBlank() && !loading,
                modifier = Modifier.fillMaxWidth(),
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent, contentColor = Palette.TextPrimary),
            ) { Text(if (loading) "Signing in…" else "Sign in") }

            if (githubEnabled) {
                Text("or", color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium, modifier = Modifier.align(Alignment.CenterHorizontally))
                OutlinedButton(
                    onClick = { GitHubAuth.launch(ctx, settings.baseUrl) },
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Icon(Icons.Filled.Code, contentDescription = null)
                    Spacer(Modifier.height(0.dp))
                    Text("  Sign in with GitHub")
                }
            }
        }
    }
}

@Composable
private fun loginFieldColors() = TextFieldDefaults.colors(
    focusedContainerColor = Palette.Card,
    unfocusedContainerColor = Palette.Card,
    focusedTextColor = Palette.TextPrimary,
    unfocusedTextColor = Palette.TextPrimary,
    focusedLabelColor = Palette.TextSecondary,
    unfocusedLabelColor = Palette.TextTertiary,
    cursorColor = Palette.Accent,
)
