package sh.sandboxed.dashboard.ui.fido

import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.data.FidoSignRequest
import sh.sandboxed.dashboard.ui.theme.Palette

@Composable
fun FidoOverlay(container: AppContainer, host: FragmentActivity?) {
    val pending: List<FidoSignRequest> by container.fido.pending.collectAsState()
    val current: FidoSignRequest = pending.firstOrNull() ?: return
    FidoApprovalDialog(req = current, host = host, scope = container.scope) { approved ->
        container.scope.launch {
            runCatching { container.api.fidoRespond(current.requestId, approved) }
            container.fido.resolve(current.requestId)
        }
    }
}

@Composable
private fun FidoApprovalDialog(
    req: FidoSignRequest,
    host: FragmentActivity?,
    scope: CoroutineScope,
    onResult: (Boolean) -> Unit,
) {
    AlertDialog(
        onDismissRequest = { /* require explicit decision */ },
        title = { Text("Approve signing request?", color = Palette.TextPrimary) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(Modifier.fillMaxWidth()) {
                    Text("Origin: ", color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
                    Text(req.origin, color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                req.hostname?.let {
                    Row(Modifier.fillMaxWidth()) {
                        Text("Host: ", color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
                        Text(it, color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium)
                    }
                }
                req.workspace?.let {
                    Row(Modifier.fillMaxWidth()) {
                        Text("Workspace: ", color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
                        Text(it, color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium)
                    }
                }
                Row(Modifier.fillMaxWidth()) {
                    Text("Key: ", color = Palette.TextTertiary, style = MaterialTheme.typography.labelMedium)
                    Text(req.keyType, color = Palette.TextPrimary, style = MaterialTheme.typography.bodyMedium)
                }
                Spacer(Modifier.height(4.dp))
                Text(req.keyFingerprint, color = Palette.TextSecondary, style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace))
            }
        },
        confirmButton = {
            Button(
                onClick = {
                    requestBiometric(host) { ok ->
                        if (ok) onResult(true) else onResult(false)
                    }
                },
                colors = ButtonDefaults.buttonColors(containerColor = Palette.Accent),
            ) { Text("Approve") }
        },
        dismissButton = {
            OutlinedButton(onClick = { onResult(false) }) { Text("Deny") }
        },
        containerColor = Palette.Card,
    )
}

private fun requestBiometric(host: FragmentActivity?, onResult: (Boolean) -> Unit) {
    val activity = host
    if (activity == null) { onResult(false); return }
    val mgr = BiometricManager.from(activity)
    val auth = mgr.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_WEAK or BiometricManager.Authenticators.DEVICE_CREDENTIAL)
    if (auth != BiometricManager.BIOMETRIC_SUCCESS) {
        onResult(false); return
    }
    val executor = ContextCompat.getMainExecutor(activity)
    val prompt = BiometricPrompt(activity, executor, object : BiometricPrompt.AuthenticationCallback() {
        override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) { onResult(true) }
        override fun onAuthenticationError(errorCode: Int, errString: CharSequence) { onResult(false) }
    })
    val info = BiometricPrompt.PromptInfo.Builder()
        .setTitle("Confirm signing")
        .setSubtitle("Authenticate to approve the signing request")
        .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_WEAK or BiometricManager.Authenticators.DEVICE_CREDENTIAL)
        .build()
    prompt.authenticate(info)
}
