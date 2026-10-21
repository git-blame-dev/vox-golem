package dev.gitblame.voxgolem.capture

/** Permission-dialog pause is not abandonment; STOP is. Settlement waits for RESUME. */
internal class CaptureEnableRequest<T>(private val cancelled: (T) -> Unit) {
    private var pending: T? = null
    private var granted = false
    fun begin(value: T) { cancel(); pending = value; granted = false }
    fun permissionResult(value: T) { if (pending === value) granted = true }
    fun settle(visible: Boolean, alive: Boolean, permitted: Boolean, enable: (T) -> Unit) {
        if (!alive) { cancel(); return }
        if (!visible || !granted) return
        val value = pending ?: return
        pending = null
        if (permitted) enable(value) else cancelled(value)
    }
    fun cancel() { val value = pending; pending = null; granted = false; if (value != null) cancelled(value) }
}
