pluginManagement {
    repositories {
        google()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
        // The Java part of rustls-platform-verifier, which checks the server's
        // certificate with Android's own verifier. It is published only here.
        maven("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
    }
}

rootProject.name = "fonoteca"
