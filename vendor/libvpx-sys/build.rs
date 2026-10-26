//! Link the VENDORED static libvpx `scripts/vpx-source.sh` built, and bindgen its public encoder + decoder
//! surface (VP8 + VP9). There is NO pkg-config / system-libvpx branch by design (J6/I6): `VPX_LIB_DIR` and
//! `VPX_INCLUDE_DIR` (set by `.cargo/config.toml`, pointing at `third_party/libvpx-chrome`) are REQUIRED.
//! This `build.rs` runs in the DEFAULT build now -- korvus-render links libvpx for VP9/VP8 `<video>` decode
//! (D144) -- as well as under korvus-runtime's `webrtc-media` feature (the VP8 send encoder).
use std::env;
use std::path::PathBuf;

fn main() {
    let lib_dir = env::var("VPX_LIB_DIR").expect(
        "VPX_LIB_DIR must point at the vendored libvpx (run scripts/vpx-source.sh); this crate never links system libvpx",
    );
    let include_dir = env::var("VPX_INCLUDE_DIR").expect(
        "VPX_INCLUDE_DIR must point at the vendored libvpx headers (run scripts/vpx-source.sh)",
    );
    // A clear error before the linker's cryptic "cannot find -lvpx": the env vars are set by
    // .cargo/config.toml, but the library only exists once scripts/vpx-source.sh has built + linked it.
    let archive = PathBuf::from(&lib_dir).join("libvpx.a");
    if !archive.exists() {
        panic!(
            "libvpx is not built: {} is missing. Run scripts/vpx-source.sh before building \
             (D144: the default build links libvpx for VP9/VP8 <video> decode).",
            archive.display()
        );
    }
    println!("cargo:rerun-if-env-changed=VPX_LIB_DIR");
    println!("cargo:rerun-if-env-changed=VPX_INCLUDE_DIR");
    println!("cargo:rerun-if-changed=wrapper.h");
    // Rust BUNDLES this static lib into the rlib (static=vpx default is +bundle), so a rebuilt libvpx
    // (e.g. VP8-only -> VP9, D144) must re-run this build.rs or a `clean: false` CI runner links the stale
    // bundled copy -> `vpx_codec_vp9_dx` undefined. Tracking the archive (the symlink's target) forces that.
    println!("cargo:rerun-if-changed={}", archive.display());
    println!("cargo:rustc-link-search=native={lib_dir}");
    // The static lib is named libvpx.a (link name `vpx`), as vpx-source.sh installs it.
    println!("cargo:rustc-link-lib=static=vpx");

    let bindings = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{include_dir}"))
        // The encoder AND decoder surface (`vpx_codec_*` covers vpx_codec_decode / get_frame /
        // dec_init_ver / vp8_dx / vp9_dx), plus the image/codec types and VPX_* constants.
        .allowlist_function("vpx_codec_.*")
        .allowlist_function("vpx_img_.*")
        .allowlist_type("vpx_codec_.*")
        .allowlist_type("vpx_image.*")
        .allowlist_type("vpx_enc_.*")
        .allowlist_type("vpx_rational.*")
        // The VP8 encoder control ids + token-partition enum (vp8e_enc_control_id / vp8e_token_partitions):
        // their VP8E_*/VP8_* names are enum VARIANTS, so the TYPE must be allowlisted to emit them.
        .allowlist_type("vp8e_.*")
        .allowlist_var("VPX_.*")
        .allowlist_var("VP8E_.*")
        .layout_tests(false)
        .generate()
        .expect("bindgen over the vendored libvpx public headers");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings.write_to_file(out.join("bindings.rs")).expect("write libvpx bindings");
}
