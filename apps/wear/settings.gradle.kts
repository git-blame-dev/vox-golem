pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "VoxGolemWear"
include(":app")

include(":voice-android")
project(":voice-android").projectDir = file("../../crates/voice-android")

include(":capture-android")
project(":capture-android").projectDir = file("../../crates/capture-android")

gradle.extensions.extraProperties["assistantNativeAbis"] = listOf("armeabi-v7a")
include(":assistant-android")
project(":assistant-android").projectDir = file("../../crates/assistant-android")
