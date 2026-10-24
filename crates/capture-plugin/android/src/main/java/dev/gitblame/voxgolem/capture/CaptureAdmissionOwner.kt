package dev.gitblame.voxgolem.capture

/** Shell scope may decline capture effects without inspecting or changing the user's capture state. */
interface CaptureAdmissionOwner {
    val captureAdmissionEnabled: Boolean
}
