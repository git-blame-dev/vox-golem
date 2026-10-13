package dev.gitblame.voxgolem.assistant.speech

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.speech.RecognitionService
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech

object SpeechServiceRequirement {
    const val GOOGLE_SPEECH_PACKAGE = "com.google.android.tts"
    const val PLAY_STORE_MARKET_URI = "market://details?id=$GOOGLE_SPEECH_PACKAGE"
    const val PLAY_STORE_WEB_URI = "https://play.google.com/store/apps/details?id=$GOOGLE_SPEECH_PACKAGE"
    const val REQUIREMENT_MESSAGE = "Speech Recognition & Synthesis by Google is required for local voice dictation and speech output."

    fun isSttInstalled(context: Context): Boolean {
        val recognizers = runCatching {
            context.packageManager.queryIntentServices(Intent(RecognitionService.SERVICE_INTERFACE), 0)
        }.getOrNull()
        return !recognizers.isNullOrEmpty() || runCatching { SpeechRecognizer.isRecognitionAvailable(context) }.getOrDefault(false)
    }

    fun isTtsInstalled(context: Context): Boolean {
        val ttsEngines = runCatching {
            context.packageManager.queryIntentServices(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), 0)
        }.getOrNull()
        return !ttsEngines.isNullOrEmpty()
    }

    fun isMissing(context: Context): Boolean {
        return !isSttInstalled(context) || !isTtsInstalled(context)
    }

    fun missingRequirementDescription(context: Context): String? {
        val stt = isSttInstalled(context)
        val tts = isTtsInstalled(context)
        return when {
            !stt && !tts -> "Speech Recognition & Synthesis by Google is required for local voice dictation and speech output."
            !stt -> "Speech Recognition & Synthesis by Google is required for local voice dictation."
            !tts -> "Speech Recognition & Synthesis by Google is required for local speech output."
            else -> null
        }
    }

    fun openPlayStore(context: Context): Boolean {
        val marketIntent = Intent(Intent.ACTION_VIEW, Uri.parse(PLAY_STORE_MARKET_URI)).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        return try {
            val playIntent = Intent(marketIntent).setPackage("com.android.vending")
            context.startActivity(playIntent)
            true
        } catch (_: Exception) {
            try {
                context.startActivity(marketIntent)
                true
            } catch (_: Exception) {
                try {
                    val webIntent = Intent(Intent.ACTION_VIEW, Uri.parse(PLAY_STORE_WEB_URI)).apply {
                        addCategory(Intent.CATEGORY_BROWSABLE)
                        addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    }
                    context.startActivity(webIntent)
                    true
                } catch (_: Exception) {
                    false
                }
            }
        }
    }
}
