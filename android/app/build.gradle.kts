import java.util.Properties
import java.io.FileInputStream

// Load before the android { } block
val keystoreProperties = Properties()
val keystorePropertiesFile = rootProject.file("keystore.properties")
if (keystorePropertiesFile.exists()) {
    keystoreProperties.load(FileInputStream(keystorePropertiesFile))
}

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
    signingConfigs {
        create("release") {
            storeFile = file(keystoreProperties["storeFile"] as String)
            storePassword = keystoreProperties["storePassword"] as String
            keyAlias = keystoreProperties["keyAlias"] as String
            keyPassword = keystoreProperties["keyPassword"] as String
        }
    }

    buildTypes {
	getByName("release") {
            signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = true   // optional
            // proguardFiles(...)
        }
        release {
            isMinifyEnabled = false
            // Signing: generate a keystore once (see android/README.md) and
            // wire a signingConfig here; debug builds sign automatically.
        }
    }
}
