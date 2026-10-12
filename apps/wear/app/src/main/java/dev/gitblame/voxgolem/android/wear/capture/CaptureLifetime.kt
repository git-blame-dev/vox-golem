package dev.gitblame.voxgolem.android.wear.capture

import java.util.concurrent.atomic.AtomicBoolean

/** Process-local file/admission ownership; Service destruction does not imply worker retirement. */
internal object CaptureLifetime {
    private var active: Owner? = null

    @Synchronized fun tryAcquire(): Owner? =
        if (active != null) null else Owner().also { active = it }

    @Synchronized fun current(): Owner? = active

    @Synchronized fun ownsCapture(captureId: String): Boolean = active?.ownsCapture(captureId) == true

    class Owner internal constructor() : AutoCloseable {
        private var references = 1
        private var retired = false
        private var captureId: String? = null

        fun bindCapture(id: String) = synchronized(CaptureLifetime) {
            check(active === this && !retired && captureId == null)
            captureId = id
        }

        fun permitsRecovery(): Boolean = synchronized(CaptureLifetime) {
            active === this && captureId == null
        }

        internal fun ownsCapture(id: String): Boolean = synchronized(CaptureLifetime) { captureId == id }

        /** Close this hold on actual coroutine completion, not on its cancellation request. */
        fun retain(): AutoCloseable = synchronized(CaptureLifetime) {
            check(active === this && !retired)
            references++
            val released = AtomicBoolean(false)
            AutoCloseable {
                if (released.compareAndSet(false, true)) synchronized(CaptureLifetime) { releaseReference() }
            }
        }

        override fun close() = synchronized(CaptureLifetime) {
            if (!retired) {
                retired = true
                releaseReference()
            }
        }

        private fun releaseReference() {
            check(references > 0)
            references--
            if (references == 0 && active === this) active = null
        }
    }
}
