package sh.sandboxed.dashboard.util

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import sh.sandboxed.dashboard.data.AppContainer

/**
 * Lightweight haptic helper. minSdk=26 so [VibrationEffect.createOneShot] is always
 * available; predefined effects (TICK / CLICK / HEAVY_CLICK) require API 29+ and are
 * gated accordingly with one-shot fallbacks of varying intensity.
 */
class Haptics(container: AppContainer) {
    private val vibrator: Vibrator? = run {
        val ctx: Context = container.appContext
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val mgr = ctx.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager
            mgr?.defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            ctx.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
        }
    }

    fun light() = oneShot(intensity = 32, durationMs = 12)
    fun medium() = oneShot(intensity = 96, durationMs = 18)
    fun success() = predefinedOrFallback(legacyDuration = 24, intensity = 160)
    fun selection() = oneShot(intensity = 24, durationMs = 8)
    fun error() {
        val v = vibrator ?: return
        v.vibrate(VibrationEffect.createWaveform(longArrayOf(0, 25, 35, 25), -1))
    }

    private fun oneShot(intensity: Int, durationMs: Long) {
        val v = vibrator ?: return
        v.vibrate(VibrationEffect.createOneShot(durationMs, intensity.coerceIn(1, 255)))
    }

    private fun predefinedOrFallback(legacyDuration: Long, intensity: Int) {
        val v = vibrator ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            v.vibrate(VibrationEffect.createPredefined(VibrationEffect.EFFECT_HEAVY_CLICK))
        } else {
            v.vibrate(VibrationEffect.createOneShot(legacyDuration, intensity.coerceIn(1, 255)))
        }
    }
}
