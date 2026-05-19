-keepattributes *Annotation*, InnerClasses
-dontwarn kotlinx.serialization.**
-keep,includedescriptorclasses class sh.sandboxed.dashboard.**$$serializer { *; }
-keepclassmembers class sh.sandboxed.dashboard.** {
    *** Companion;
    kotlinx.serialization.KSerializer serializer(...);
}
-keep class sh.sandboxed.dashboard.data.** { *; }
