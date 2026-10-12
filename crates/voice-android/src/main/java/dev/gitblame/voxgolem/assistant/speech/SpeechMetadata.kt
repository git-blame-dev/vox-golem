package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.RecognitionRoute
import org.json.JSONArray
import org.json.JSONObject

fun RecognitionRoute.metadata() = JSONObject().put("selectedProvider", provider)
    .put("requestedLanguage", requestedLanguage).put("selectedLanguage", selectedLanguage ?: JSONObject.NULL)
    .put("reportedLanguage", reportedLanguage ?: JSONObject.NULL)
    .put("installedLanguages", installedLanguages?.let(::JSONArray) ?: JSONObject.NULL)
    .put("supportedLanguages", supportedLanguages?.let(::JSONArray) ?: JSONObject.NULL)
    .put("actualProcessing", if (onDeviceOnly) "on_device_api" else "not_reported")
    .put("localPreferred", localPreferred).put("fallbackReason", fallbackReason ?: JSONObject.NULL)
    .put("model", JSONObject.NULL).put("offlineTrialVerified", false)

fun TtsRoute.metadata() = JSONObject().put("configuredProvider", if (providerIsPreference) provider ?: JSONObject.NULL else JSONObject.NULL)
    .put("reportedProvider", if (providerIsPreference) JSONObject.NULL else provider ?: JSONObject.NULL)
    .put("requestedLanguage", requestedLanguage).put("selectedLanguage", selectedLanguage)
    .put("selectedVoice", selectedVoice ?: JSONObject.NULL).put("reportedLanguage", JSONObject.NULL)
    .put("installedLanguages", installedLanguages?.let(::JSONArray) ?: JSONObject.NULL)
    .put("networkRequired", networkRequired).put("actualProcessing", "not_reported")
    .put("fallbackReason", fallbackReason ?: JSONObject.NULL).put("model", JSONObject.NULL).put("offlineTrialVerified", false)
