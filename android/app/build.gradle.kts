plugins {
    id("com.android.application")
}

android {
    namespace = "ie.cannygreen.berapp"
    compileSdk = 35

    defaultConfig {
        applicationId = "ie.cannygreen.berapp"
        minSdk = 30
        targetSdk = 35
        versionCode = 1
        versionName = "0.0.1"
        // arm64 only — modern tablets; the Rust side only builds this target.
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Signing: generate a keystore once (see android/README.md) and
            // wire a signingConfig here; debug builds sign automatically.
        }
    }
}
