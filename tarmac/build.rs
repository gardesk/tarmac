fn main() {
    // SkyLight is a private framework at /System/Library/PrivateFrameworks/
    println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
    println!("cargo:rustc-link-lib=framework=SkyLight");
}
