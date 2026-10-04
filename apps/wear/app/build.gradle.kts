plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.gitblame.voxgolem.android.wear"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.gitblame.voxgolem.android"
        minSdk = 30
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release { isMinifyEnabled = false }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures { compose = true }
    composeOptions { kotlinCompilerExtensionVersion = "1.5.15" }
    testOptions { unitTests.isReturnDefaultValues = true }

}

val captureDomainSources = layout.buildDirectory.dir("generated/capture-domain")
val prepareCaptureDomainSources by tasks.registering(Sync::class) {
    from(rootProject.file("../../crates/capture-plugin/android/src/main/java")) {
        include(
            "dev/gitblame/voxgolem/capture/CaptureContract.kt",
            "dev/gitblame/voxgolem/capture/CaptureRepository.kt",
            "dev/gitblame/voxgolem/capture/PcmRecordingWriter.kt",
        )
    }
    into(captureDomainSources)
}
android.sourceSets.getByName("main").java.srcDir(captureDomainSources)
tasks.named("preBuild").configure { dependsOn(prepareCaptureDomainSources) }

dependencies {
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.fragment:fragment-ktx:1.8.6")
    implementation("androidx.compose.ui:ui:1.7.8")
    implementation("androidx.compose.material:material:1.7.8")
    implementation("androidx.compose.ui:ui-tooling-preview:1.7.8")
    implementation("androidx.wear.compose:compose-material:1.4.1")
    implementation("androidx.wear.compose:compose-foundation:1.4.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("com.google.android.gms:play-services-wearable:19.0.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.6.1")

}
