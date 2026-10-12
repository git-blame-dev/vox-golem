extensions.configure<com.android.build.gradle.AppExtension> {
    sourceSets.getByName("main").res.srcDir(file("../../../../shell-res"))
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
        isCoreLibraryDesugaringEnabled = true
    }
    if (providers.gradleProperty("voxgolemCaptureTiming").orNull == "true") {
        sourceSets.getByName("release").manifest.srcFile(file("../../../../capture-performance/AndroidManifest.xml"))
    }
}

dependencies {
    add("implementation", project(":tauri-plugin-assistant"))
    add("coreLibraryDesugaring", "com.android.tools:desugar_jdk_libs:2.1.5")
}

val voiceShortcutResources = layout.buildDirectory.dir("generated/voice-shortcut-res")
val prepareVoiceShortcutResources by tasks.registering(Exec::class) {
    val generator = rootProject.file("../../../../../scripts/generate-icons.py")
    inputs.files(generator, rootProject.file("../../../../../apps/desktop-tauri/src-tauri/icons/icon.svg"))
    outputs.dir(voiceShortcutResources)
    commandLine("python3", generator.absolutePath, "--phone-shortcut-res-output", voiceShortcutResources.get().asFile.absolutePath)
}
extensions.configure<com.android.build.gradle.AppExtension> {
    sourceSets.getByName("main").res.srcDir(voiceShortcutResources)
}
tasks.named("preBuild").configure { dependsOn(prepareVoiceShortcutResources) }
