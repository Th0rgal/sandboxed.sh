package sh.sandboxed.dashboard.util

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.browser.customtabs.CustomTabsIntent
import androidx.core.net.toUri

/**
 * Launches the GitHub OAuth flow in a Custom Tab pointed at the backend's
 * `/api/auth/github/start` route. The backend is expected to redirect to a
 * `sandboxed://auth/callback?token=<jwt>&exp=<unix_ts>` URI on success
 * (or `?error=<message>` on failure), which is intercepted by [MainActivity].
 *
 * Backend contract assumed (not yet implemented server-side):
 *  - `GET /api/health` returns `github_enabled: true` when the server has a
 *    GitHub OAuth App configured.
 *  - `GET /api/auth/github/start?redirect=sandboxed%3A%2F%2Fauth%2Fcallback`
 *    302s to GitHub's authorize URL, sets a state cookie scoped to the host.
 *  - `GET /api/auth/github/callback?code=&state=` exchanges the code with
 *    GitHub, fetches the user, looks up or provisions a `UserAccount`,
 *    issues a JWT, and 302s to the supplied redirect URI with `token` +
 *    `exp` query parameters.
 */
object GitHubAuth {
    const val REDIRECT_SCHEME = "sandboxed"
    const val REDIRECT_HOST = "auth"
    const val REDIRECT_PATH = "/callback"
    const val REDIRECT_URI = "$REDIRECT_SCHEME://$REDIRECT_HOST$REDIRECT_PATH"

    fun launch(context: Context, baseUrl: String) {
        val authorizeUrl = "${baseUrl.trimEnd('/')}/api/auth/github/start" +
                "?redirect=" + Uri.encode(REDIRECT_URI)
        val tabsIntent = CustomTabsIntent.Builder()
            .setShowTitle(true)
            .setUrlBarHidingEnabled(false)
            .build()
        tabsIntent.intent.flags = Intent.FLAG_ACTIVITY_NEW_TASK
        runCatching { tabsIntent.launchUrl(context, authorizeUrl.toUri()) }
    }

    /** True iff the intent looks like our OAuth callback. */
    fun isCallback(uri: Uri?): Boolean {
        uri ?: return false
        return uri.scheme == REDIRECT_SCHEME && uri.host == REDIRECT_HOST && uri.path == REDIRECT_PATH
    }

    data class CallbackResult(val token: String?, val exp: Long?, val error: String?)

    fun parse(uri: Uri): CallbackResult = CallbackResult(
        token = uri.getQueryParameter("token"),
        exp = uri.getQueryParameter("exp")?.toLongOrNull(),
        error = uri.getQueryParameter("error"),
    )
}
