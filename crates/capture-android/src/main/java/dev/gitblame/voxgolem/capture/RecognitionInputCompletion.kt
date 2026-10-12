package dev.gitblame.voxgolem.capture

class RecognitionInputCompletion {
    private val lock = Any()
    private var eofDelivered = false

    fun closeAfterDrain(closeInput: () -> Unit) = synchronized(lock) {
        closeInput()
        eofDelivered = true
    }

    fun canAcceptFinal(): Boolean = synchronized(lock) { eofDelivered }
}
