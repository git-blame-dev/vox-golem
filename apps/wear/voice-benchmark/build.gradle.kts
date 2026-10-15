plugins {
    id("com.android.application")
}

android {
    namespace = "dev.gitblame.voxgolem.android.test"
    compileSdk {
        version = release(37) { minorApiLevel = 0 }
    }
    defaultConfig {
        applicationId = "dev.gitblame.voxgolem.android.test"
        minSdk = 30
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"
    }
    buildTypes {
        create("performance") {
            isDebuggable = true
            signingConfig = signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

androidComponents.beforeVariants {
    if (it.buildType != "performance") it.enable = false
}
