package sh.sandboxed.dashboard

import android.app.Application
import sh.sandboxed.dashboard.data.AppContainer

class SandboxedDashboardApp : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
    }
}
