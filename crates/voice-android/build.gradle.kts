import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.OutputDirectory

plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

layout.buildDirectory.set(rootProject.layout.buildDirectory.dir("voice-android"))

abstract class GenerateListeningCueResources : Exec() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty
}

val listeningCueResources = layout.buildDirectory.dir("generated/listening-cue/res")
val stageListeningCue = tasks.register<GenerateListeningCueResources>("stageListeningCue") {
    val checkout = projectDir.parentFile.parentFile
    val source = checkout.resolve("frontend/app/public/resources")
    val generator = checkout.resolve("scripts/generate-mobile-cues.py")
    inputs.files(generator, source.resolve("start-listening.wav"), source.resolve("stop-listening.wav"))
    outputDirectory.set(listeningCueResources)
    doFirst {
        commandLine("python3", generator.absolutePath, "--source-dir", source.absolutePath,
            "--output-dir", outputDirectory.get().asFile.absolutePath)
    }
}

android {
    namespace = "dev.gitblame.voxgolem.voice"
    compileSdk = 36
    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions { jvmTarget = "1.8" }
    buildFeatures { buildConfig = false }
    testOptions { unitTests.isIncludeAndroidResources = true }
}

androidComponents.onVariants { variant ->
    requireNotNull(variant.sources.res).addGeneratedSourceDirectory(stageListeningCue) { it.outputDirectory }
}
tasks.named("preBuild").configure { dependsOn(stageListeningCue) }

dependencies {
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
}
