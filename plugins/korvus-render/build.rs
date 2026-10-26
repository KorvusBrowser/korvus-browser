//! Builds the vendored fdlibm (`third_party/fdlibm`, see its README.korvus) as Chromium does: no floating-point
//! contraction -- so what it computes here is the bytes Chrome's is. (Web Audio's FFT is no longer PFFFT since
//! Chrome 152: `src/audio/rust_fft.rs`.)
fn main() {
    println!("cargo:rerun-if-changed=third_party/fdlibm");
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .include("third_party/fdlibm/include")
        .file("third_party/fdlibm/ieee754.cc")
        .file("third_party/fdlibm/korvus_fdlibm.cc")
        .flag_if_supported("-ffp-contract=off")
        .opt_level(2)
        .warnings(false)
        .compile("korvus_fdlibm");
}
