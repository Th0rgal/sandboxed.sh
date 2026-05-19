package sh.sandboxed.dashboard

import android.content.Intent
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.fragment.app.FragmentActivity
import kotlinx.coroutines.launch
import sh.sandboxed.dashboard.data.AppContainer
import sh.sandboxed.dashboard.ui.nav.AppRoot
import sh.sandboxed.dashboard.ui.theme.SandboxedTheme
import sh.sandboxed.dashboard.util.GitHubAuth

class MainActivity : FragmentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val container = (application as SandboxedDashboardApp).container
        handleAuthIntent(intent, container)
        setContent {
            SandboxedTheme {
                val settings by container.cached.collectAsState()
                AppRoot(container = container, settings = settings, host = this)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val container = (application as SandboxedDashboardApp).container
        handleAuthIntent(intent, container)
    }

    private fun handleAuthIntent(intent: Intent?, container: AppContainer) {
        val data = intent?.data ?: return
        if (!GitHubAuth.isCallback(data)) return
        val result = GitHubAuth.parse(data)
        val token = result.token ?: return
        container.scope.launch { container.settings.setToken(token) }
    }
}
