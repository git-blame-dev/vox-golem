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

// The app's prepareCaptureDomainSources task shares only the maintained capture
// domain and storage sources. Phone Tauri services and capabilities stay on phone.
