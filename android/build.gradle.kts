// Packages the Rust library, built beforehand into src/main/jniLibs (see
// .github/workflows/rolling-release.yml), as an app that opens it in a
// NativeActivity.

plugins {
    id("com.android.application") version "9.4.1"
}

// Rolling builds pass their version, such as 0.0.0-dev.12, and are installed
// as an app of their own beside a real release.
val appVersion = providers.gradleProperty("appVersion").getOrElse("0.0.0-dev")
val dev = "-dev" in appVersion

// The Java part of rustls-platform-verifier must be the same version as the
// crate in Cargo.lock.
val verifierVersion = file("../Cargo.lock").readLines().let { lines ->
    val at = lines.indexOf("name = \"rustls-platform-verifier-android\"")
    check(at >= 0) { "rustls-platform-verifier-android is not in Cargo.lock" }
    lines[at + 1].substringAfter('"').substringBefore('"')
}

android {
    namespace = "io.github.schotman_development.fonoteca"
    compileSdk = 36
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "io.github.schotman_development.fonoteca" + if (dev) ".dev" else ""
        minSdk = 28
        targetSdk = 36
        versionCode = providers.gradleProperty("appVersionCode").getOrElse("1").toInt()
        versionName = appVersion
        manifestPlaceholders["label"] = if (dev) "Fonoteca Dev" else "Fonoteca"
    }

    signingConfigs {
        create("release") {
            storeFile = System.getenv("SIGNING_STORE_FILE")?.let(::file)
            storePassword = System.getenv("SIGNING_STORE_PASSWORD")
            keyAlias = System.getenv("SIGNING_KEY_ALIAS")
            keyPassword = System.getenv("SIGNING_KEY_PASSWORD")
        }
    }

    buildTypes {
        getByName("release") {
            signingConfig = signingConfigs.getByName("release")
        }
    }
}

dependencies {
    implementation("org.rustls:rustls-platform-verifier:$verifierVersion")
}
