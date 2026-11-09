extensions.configure<com.android.build.gradle.AppExtension> {
    ndkVersion = "28.2.13676358"
    sourceSets.getByName("main").res.srcDir(file("../../../../shell-res"))
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
        isCoreLibraryDesugaringEnabled = true
    }
    if (providers.gradleProperty("voxgolemSyntheticVoiceBenchmark").orNull == "true") {
        sourceSets.getByName("release").manifest.srcFile(file("../../../../benchmark/AndroidManifest.xml"))
    } else if (providers.gradleProperty("voxgolemCaptureTiming").orNull != "false") {
        sourceSets.getByName("release").manifest.srcFile(file("../../../../capture-performance/AndroidManifest.xml"))
    }
}

dependencies {
    add("implementation", project(":tauri-plugin-assistant"))
    add("coreLibraryDesugaring", "com.android.tools:desugar_jdk_libs:2.1.5")
}

// Agy is Google's proprietary Antigravity executable: it is never committed or redistributed.
// Personal builds supply it from one directory outside the repository (<arm64-v8a>/libagy.so):
// VOXGOLEM_AGY_DIR, else <repo>/../dependencies/agy. Without it the build simply omits Agy and the
// Agy models report unavailable; set VOXGOLEM_REQUIRE_AGY=1 to fail the build instead.
val agyDir = providers.environmentVariable("VOXGOLEM_AGY_DIR").orNull?.let(::File)
    ?: rootProject.file("../../../../../../dependencies/agy")
if (agyDir.resolve("arm64-v8a/libagy.so").isFile) {
    extensions.configure<com.android.build.gradle.AppExtension> { sourceSets.getByName("main").jniLibs.srcDir(agyDir) }
} else if (providers.environmentVariable("VOXGOLEM_REQUIRE_AGY").orNull == "1") {
    throw GradleException("VOXGOLEM_REQUIRE_AGY=1 but ${agyDir.path}/arm64-v8a/libagy.so is missing")
} else {
    logger.warn("Agy binary not found at ${agyDir.path}; building without Agy")
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
