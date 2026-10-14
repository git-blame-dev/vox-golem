import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.Internal

plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

layout.buildDirectory.set(rootProject.layout.buildDirectory.dir("assistant-android"))

abstract class StageAssistantNativeLibraries : Sync() {
    // Sync already declares its destination as an output directory.
    @get:Internal
    abstract val outputDirectory: DirectoryProperty
}

android {
    namespace = "dev.gitblame.voxgolem.assistant"
    compileSdk = 36
    ndkVersion = "28.2.13676358"
    defaultConfig { minSdk = 24 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions { jvmTarget = "1.8" }
    buildFeatures { buildConfig = false }
    testOptions { unitTests.isIncludeAndroidResources = true }
    defaultConfig { consumerProguardFiles("consumer-rules.pro") }
}

dependencies {
    api(project(":voice-android"))
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("com.google.ai.edge.aicore:aicore:0.0.1-exp02")
    implementation("com.google.android.gms:play-services-wearable:19.0.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
}

tasks.withType<Test>().configureEach {
    dependsOn("buildAssistantNativeJvm")
    val library = providers.gradleProperty("assistantNativeLibrary").orNull?.let(::file)
        ?: layout.buildDirectory.file("rust-native/debug/libwear_assistant_native.so").get().asFile
    systemProperty("assistant.native.library", library.absolutePath)
    systemProperty("java.library.path", library.parent)
}

val nativeCheckout = projectDir.parentFile.parentFile
val nativeBuildDirectory = layout.buildDirectory.dir("rust-native")
val nativeAbis = providers.gradleProperty("assistantNativeAbis").orNull?.split(',')
    ?: (gradle.extensions.extraProperties.properties["assistantNativeAbis"] as? List<*>)?.map { it.toString() }
    ?: throw GradleException("Set assistantNativeAbis for this Android shell")
val nativeTargets = mapOf("armeabi-v7a" to "armv7-linux-androideabi", "arm64-v8a" to "aarch64-linux-android")
nativeAbis.forEach { require(it in nativeTargets) { "Unsupported assistant native ABI" } }
android.defaultConfig.ndk.abiFilters.addAll(nativeAbis)
val nativeInputs = files(nativeCheckout.resolve("Cargo.toml"), nativeCheckout.resolve("Cargo.lock"),
    fileTree(nativeCheckout.resolve("crates/wear-assistant-native")), fileTree(nativeCheckout.resolve("crates/platform")),
    fileTree(nativeCheckout.resolve("crates/core")))

fun registerNativeBuild(name: String, target: String?, release: Boolean) = tasks.register<Exec>(name) {
    workingDir(nativeCheckout)
    inputs.files(nativeInputs)
    val profile = if (release) "release" else "debug"
    outputs.file(nativeBuildDirectory.map { it.file("${target?.let { t -> "$t/" } ?: ""}$profile/libwear_assistant_native.so") })
    val args = mutableListOf("cargo", "build", "--offline", "--locked", "-p", "voxgolem-wear-assistant-native",
        "--target-dir", nativeBuildDirectory.get().asFile.absolutePath)
    if (release) args += "--release"
    if (target != null) {
        args += listOf("--target", target)
        inputs.property("ndkHome", providers.environmentVariable("NDK_HOME").orElse(""))
        doFirst {
            val ndk = providers.environmentVariable("NDK_HOME").orNull ?: throw GradleException("Set NDK_HOME for assistant JNI")
            val tools = file("$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin")
            val compiler = tools.resolve(if (target == "armv7-linux-androideabi") "armv7a-linux-androideabi24-clang" else "aarch64-linux-android24-clang")
            if (!compiler.isFile) throw GradleException("Installed Linux NDK API24 compiler required")
            val key = target.replace('-', '_')
            environment("CARGO_TARGET_${key.uppercase()}_LINKER", compiler.absolutePath)
            environment("CC_$key", compiler.absolutePath)
            environment("AR_$key", tools.resolve("llvm-ar").absolutePath)
        }
    }
    commandLine(args)
}

registerNativeBuild("buildAssistantNativeJvm", null, false)
listOf("debug", "release").forEach { variant ->
    val title = variant.replaceFirstChar { it.uppercaseChar() }
    val builds = nativeAbis.map { abi -> registerNativeBuild("buildAssistantNative$title${abi.replace("-", "_")}", nativeTargets.getValue(abi), variant == "release") }
    val destination = layout.buildDirectory.dir("generated/assistant-jni/$variant")
    val stage = tasks.register<StageAssistantNativeLibraries>("stageAssistantNative$title") {
        dependsOn(builds)
        outputDirectory.set(destination)
        nativeAbis.forEach { abi ->
            from(nativeBuildDirectory.map { it.file("${nativeTargets.getValue(abi)}/$variant/libwear_assistant_native.so") }) { into(abi) }
        }
        into(outputDirectory)
    }
    androidComponents.onVariants(androidComponents.selector().withBuildType(variant)) { nativeVariant ->
        requireNotNull(nativeVariant.sources.jniLibs).addGeneratedSourceDirectory(stage) { it.outputDirectory }
    }
    tasks.matching { it.name == "merge${title}JniLibFolders" }.configureEach { dependsOn(stage) }
}
