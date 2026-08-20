import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// ---- Release signing --------------------------------------------------------------------
// **An APK's signature is what lets Android update it in place.** The debug key `flutter
// create` leaves behind is generated per machine, so a CI runner invents a new one on every
// build and each release refuses to install over the last: users would have to uninstall,
// losing everything on the device. So the release build takes a real keystore, from
// `android/key.properties` locally or the environment on CI. The keystore itself is never in
// the repo (`key.properties` is gitignored).
//
// Without one the build still works and falls back to the debug key — fine for a quick local
// release build, never for something handed to a user. See apps/mobile/README.md.
val keystoreProperties = Properties().apply {
    val file = rootProject.file("key.properties")
    if (file.exists()) file.inputStream().use { load(it) }
}

fun signingSetting(key: String, env: String): String? =
    keystoreProperties.getProperty(key) ?: System.getenv(env)

val releaseStoreFile = signingSetting("storeFile", "YMEMO_KEYSTORE_FILE")?.let { file(it) }
val releaseStorePassword = signingSetting("storePassword", "YMEMO_KEYSTORE_PASSWORD")
val releaseKeyAlias = signingSetting("keyAlias", "YMEMO_KEY_ALIAS")
// A keystore made by `keytool -genkeypair` usually reuses the store password for the key.
val releaseKeyPassword = signingSetting("keyPassword", "YMEMO_KEY_PASSWORD") ?: releaseStorePassword
val hasReleaseKeystore = releaseStoreFile?.exists() == true &&
    releaseStorePassword != null && releaseKeyAlias != null

android {
    namespace = "dev.ymemo.ymemo_mobile"
    compileSdk = flutter.compileSdkVersion
    // Pinned NDK: gradle and cargo-ndk (ANDROID_NDK_HOME) must use the **same** one.
    // Bump both together.
    ndkVersion = "28.2.13676358"

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "dev.ymemo.ymemo_mobile"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (hasReleaseKeystore) {
            create("release") {
                storeFile = releaseStoreFile
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            // No keystore: the debug key, so `flutter run --release` still works. Such a
            // build must not be handed to users — see the comment above.
            signingConfig = signingConfigs.getByName(if (hasReleaseKeystore) "release" else "debug")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
