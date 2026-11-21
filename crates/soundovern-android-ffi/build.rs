// The prebuilt Oboe static library needs the C++ runtime (e.g. __cxa_pure_virtual,
// std::runtime_error), but oboe-sys does not link it for its prebuilt libraries. Link libc++
// statically so the app ships a single .so.
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    let triple = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "aarch64-linux-android",
        Ok("arm") => "arm-linux-androideabi",
        Ok("x86_64") => "x86_64-linux-android",
        Ok("x86") => "i686-linux-android",
        _ => panic!("unsupported Android arch"),
    };
    let ndk = std::env::var("ANDROID_NDK_HOME")
        .or_else(|_| std::env::var("ANDROID_NDK_ROOT"))
        .expect("set ANDROID_NDK_HOME (cargo-ndk sets it) to locate libc++_static");
    let dir = PathBuf::from(ndk).join("toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib").join(triple);
    // Pass the archives by path: adding `dir` as a -L search path would shadow the API-level libc
    // stubs the NDK linker normally uses and break 32-bit ARM linking.
    for lib in ["libc++_static.a", "libc++abi.a"] {
        let p = dir.join(lib);
        assert!(p.exists(), "missing {}", p.display());
        println!("cargo:rustc-link-arg={}", p.display());
    }
}
