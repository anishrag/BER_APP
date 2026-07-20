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
    // Release signing only where keystore.properties exists (it's untracked);
    // without the guard the null casts break CONFIGURATION for every task —
    // even debug builds on a machine with no keystore.
    signingConfigs {
        if (keystorePropertiesFile.exists()) {
            create("release") {
                storeFile = file(keystoreProperties["storeFile"] as String)
                storePassword = keystoreProperties["storePassword"] as String
                keyAlias = keystoreProperties["keyAlias"] as String
                keyPassword = keystoreProperties["keyPassword"] as String
            }
        }
    }

    buildTypes {
        getByName("release") {
            // Signed with android/berapp.keystore via keystore.properties
            // (untracked); debug builds sign automatically.
            if (keystorePropertiesFile.exists()) {
                signingConfig = signingConfigs.getByName("release")
            }
            // No R8/minify: the app is a thin GameActivity shell over the Rust
            // cdylib — shrinking risks stripping JNI entry points for no
            // meaningful size win.
            isMinifyEnabled = false
        }
    }
}

dependencies {
    // Kotlin 1.8 folded the jdk7/jdk8 stdlib add-ons into the main kotlin-stdlib.
    // appcompat pulls kotlin-stdlib:1.8.22 while an older transitive dep still
    // pulls the split kotlin-stdlib-jdk7/jdk8:1.6.21 — so the now-merged classes
    // duplicate (checkDebugDuplicateClasses fails). Force the split artifacts up
    // to 1.8.22, where they're empty stubs that just redirect to the main
    // stdlib, and the duplicates disappear. (Same fix as the r04 CameraX build —
    // commit 6030c7a — which left with the CameraX revert.)
    constraints {
        implementation("org.jetbrains.kotlin:kotlin-stdlib-jdk7:1.8.22")
        implementation("org.jetbrains.kotlin:kotlin-stdlib-jdk8:1.8.22")
    }

    // GameActivity: the activity base class MainActivity extends. Pinned to
    // 2.0.2 — the exact version of the C glue the android-activity crate
    // (linked via winit's android-game-activity feature) vendors and
    // registers natives against; a newer AAR can declare natives the glue
    // doesn't register and abort at startup. GameTextInput (the native
    // soft-keyboard path) is bundled in this AAR.
    implementation("androidx.games:games-activity:2.0.2")
    // GameActivity extends androidx.appcompat.app.AppCompatActivity and uses
    // androidx.core insets types; games-activity 2.0.2 declares no POM
    // dependencies, so both come in via appcompat explicitly.
    implementation("androidx.appcompat:appcompat:1.7.0")
}
