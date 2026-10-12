plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

layout.buildDirectory.set(rootProject.layout.buildDirectory.dir("capture-android"))

android {
    namespace = "dev.gitblame.voxgolem.capture.shared"
    compileSdk = 36
    defaultConfig { minSdk = 24 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions { jvmTarget = "1.8" }
    buildFeatures { buildConfig = false }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
}
