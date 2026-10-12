import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.OutputDirectory

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "dev.gitblame.voxgolem.android.wear"
    compileSdk {
        version = release(37) {
            // The stable SDK package is platforms;android-37.0.
            minorApiLevel = 0
        }
    }
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "dev.gitblame.voxgolem.android"
        minSdk = 30
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk { abiFilters += "armeabi-v7a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles("assistant-proguard-rules.pro")
        }
        create("performance") {
            initWith(getByName("release"))
            matchingFallbacks += listOf("release")
            isDebuggable = false
            isMinifyEnabled = true
            isShrinkResources = true
            signingConfig = signingConfigs.getByName("debug")
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    buildFeatures { compose = true }
    testOptions {
        unitTests.isReturnDefaultValues = true
        unitTests.isIncludeAndroidResources = true
    }

}

abstract class GenerateLauncherResources : Exec() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty
}

val launcherResources = layout.buildDirectory.dir("generated/launcher-res")
val prepareLauncherResources by tasks.registering(GenerateLauncherResources::class) {
    val checkout = rootProject.file("../..")
    workingDir(checkout)
    inputs.files(checkout.resolve("scripts/generate-icons.py"), checkout.resolve("apps/desktop-tauri/src-tauri/icons/icon.svg"))
    outputDirectory.set(launcherResources)
    doFirst {
        commandLine("python3", checkout.resolve("scripts/generate-icons.py").absolutePath,
            "--wear-res-output", outputDirectory.get().asFile.absolutePath)
    }
}
androidComponents.onVariants { variant ->
    requireNotNull(variant.sources.res).addGeneratedSourceDirectory(prepareLauncherResources) { it.outputDirectory }
}
tasks.named("preBuild").configure { dependsOn(prepareLauncherResources) }

dependencies {
    implementation(project(":capture-android"))
    implementation(project(":assistant-android"))
    implementation(project(":voice-android"))
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.fragment:fragment-ktx:1.8.6")
    implementation("androidx.compose.ui:ui:1.10.0")
    implementation("androidx.compose.ui:ui-tooling-preview:1.10.0")
    implementation("androidx.wear.compose:compose-material3:1.7.1")
    implementation("androidx.wear.compose:compose-foundation:1.7.1")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.10.0")
    implementation("com.google.android.gms:play-services-wearable:19.0.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.6.1")

}

tasks.withType<Test>().configureEach {
    dependsOn(":assistant-android:buildAssistantNativeJvm")
    val libraryDirectory = rootProject.layout.buildDirectory.dir("assistant-android/rust-native/debug").get().asFile
    systemProperty("java.library.path", libraryDirectory.absolutePath)
    systemProperty("assistant.native.library", libraryDirectory.resolve("libwear_assistant_native.so").absolutePath)
}
