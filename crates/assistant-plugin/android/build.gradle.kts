plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

val benchmarkAssets = layout.buildDirectory.dir("generated/synthetic-voice-assets")
val benchmarkEnabled = providers.gradleProperty("voxgolemSyntheticVoiceBenchmark").orNull == "true"
val generateBenchmarkAssets = if (benchmarkEnabled) tasks.register<Exec>("generateSyntheticVoiceAssets") {
    val checkout = projectDir.resolve("../../..").canonicalFile
    inputs.files(checkout.resolve("tools/benchmark/voice_contract.py"), checkout.resolve("tools/benchmark/scenarios.json"))
    outputs.dir(benchmarkAssets)
    commandLine("python3", checkout.resolve("tools/benchmark/voice_contract.py").absolutePath,
        "--write-android-assets", benchmarkAssets.get().asFile.absolutePath)
} else null

android {
    if (providers.gradleProperty("voxgolemSyntheticVoiceBenchmark").orNull == "true") {
        sourceSets.getByName("main").java.srcDir("src/benchmark/java")
        sourceSets.getByName("main").java.srcDir("../../../tools/benchmark/android/src/main/java")
        defaultConfig { consumerProguardFiles("benchmark-rules.pro") }
        sourceSets.getByName("main").assets.srcDir(benchmarkAssets)
        sourceSets.getByName("test").java.srcDir("src/benchmarkTest/java")
    }
    namespace = "dev.gitblame.voxgolem.assistant.phone"
    compileSdk = 36
    defaultConfig { minSdk = 24 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions { jvmTarget = "1.8" }
}

if (generateBenchmarkAssets != null) tasks.named("preBuild").configure { dependsOn(generateBenchmarkAssets) }

dependencies {
    if (providers.gradleProperty("voxgolemSyntheticVoiceBenchmark").orNull == "true") implementation(project(":capture-android"))
    implementation(project(":tauri-android"))
    implementation(project(":assistant-android"))
    implementation(project(":voice-android"))
    implementation("com.google.android.gms:play-services-wearable:19.0.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
    testImplementation("org.robolectric:robolectric:4.14.1")
}

tasks.withType<Test>().configureEach {
    dependsOn(":assistant-android:buildAssistantNativeJvm")
    val library = providers.gradleProperty("assistantNativeLibrary").orNull?.let(::file)
        ?: rootProject.layout.buildDirectory.file("assistant-android/rust-native/debug/libwear_assistant_native.so").get().asFile
    systemProperty("assistant.native.library", library.absolutePath)
    systemProperty("java.library.path", library.parent)
}
