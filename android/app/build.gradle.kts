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
    compileOptions {
        // CameraX (and its ListenableFuture callbacks) needs Java 8+.
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
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

dependencies {
    // Kotlin 1.8 folded the jdk7/jdk8 stdlib add-ons into the main kotlin-stdlib.
    // CameraX / androidx.activity pull kotlin-stdlib:1.8.22, while an older
    // transitive dep still pulls the split kotlin-stdlib-jdk7/jdk8:1.6.21 — so the
    // now-merged classes duplicate (checkDebugDuplicateClasses fails). Force the
    // split artifacts up to 1.8.22, where they're empty stubs that just redirect
    // to the main stdlib, and the duplicates disappear.
    constraints {
        implementation("org.jetbrains.kotlin:kotlin-stdlib-jdk7:1.8.22")
        implementation("org.jetbrains.kotlin:kotlin-stdlib-jdk8:1.8.22")
    }

    // In-app camera (CameraActivity): CameraX preview + capture with an app-owned
    // flash toggle, replacing the hand-off to the system camera app (which owned
    // flash). `activity` gives CameraActivity a ComponentActivity base, which is
    // the LifecycleOwner CameraX binds its use-cases to.
    val cameraxVersion = "1.3.4"
    implementation("androidx.camera:camera-core:$cameraxVersion")
    implementation("androidx.camera:camera-camera2:$cameraxVersion")
    implementation("androidx.camera:camera-lifecycle:$cameraxVersion")
    implementation("androidx.camera:camera-view:$cameraxVersion")
    implementation("androidx.activity:activity:1.9.2")
}
