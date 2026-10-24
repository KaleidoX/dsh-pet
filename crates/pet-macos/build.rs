fn main() {
    println!("cargo:rerun-if-changed=src/native.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    cc::Build::new().file("src/native.m").flag("-fobjc-arc").flag("-Wno-deprecated-declarations").compile("pet_macos_native");
    for framework in ["Foundation", "AVFoundation", "VideoToolbox", "CoreVideo", "CoreMedia", "Metal"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
