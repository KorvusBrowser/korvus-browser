//! The GPU this PC draws with, through the ANGLE Chrome ships -- the device every WebGL context of the process shares
//! (owner 2026-10-12: "해당 PC 에 있는 hardware를 활용해야지").
//!
//! * THE LIBRARIES ARE CHROME'S REVISION: `libEGL.so` / `libGLESv2.so` built from ANGLE's own repository at the
//!   revision Chrome ships (`scripts/angle-source.sh`), so the shader translator, the WebGL validation and the driver
//!   workarounds are the ones Chrome runs: on this box they answer every WebGL probe as the installed Chrome's own
//!   ANGLE does, a bot-detection script's GPU float fingerprint 160/160 included. Found by [`angle_dir`].
//! * THE BACK END IS CHROME'S ON A LINUX DESKTOP: ANGLE's GLES back end over the native EGL driver
//!   (`ui/gl/gl_display.cc` `ANGLE_OPENGLES_EGL`: `EGL_PLATFORM_ANGLE_TYPE_OPENGLES_ANGLE` +
//!   `EGL_PLATFORM_ANGLE_DEVICE_TYPE_EGL_ANGLE`), surfaceless -- korvus has no window; Chrome's contexts draw into
//!   framebuffers too. Desktop Chrome 150 on this box reports `ANGLE (NVIDIA Corporation, NVIDIA GeForce RTX
//!   4060/PCIe/SSE2, OpenGL ES 3.2)`; so does this device.
//! * THE GPU IS THE DISPLAY'S: Chrome draws on the GPU its window is on. With two GPUs (this box: the RTX 4060
//!   driving the monitor, an AMD iGPU without one), glvnd's default EGL vendor is Mesa -- the iGPU -- whose
//!   arithmetic differs (PX's fingerprint ends 128 where Chrome's ends 127). The card with a connected connector
//!   (else the boot VGA card) names the glvnd vendor library EGL loads.
//! * NO USABLE GPU, NO DEVICE: no card, ANGLE refusing, or a software renderer (llvmpipe, softpipe, SwiftShader)
//!   -> `None`, and WebGL is unavailable as in Chrome 150 on Linux, which no longer falls back to SwiftShader
//!   (`ui/gl/gl_features.cc` `kAllowSwiftShaderFallback`, disabled by default).

use std::ffi::{c_void, CStr, CString};

pub(crate) type EGLDisplay = *mut c_void;
pub(crate) type EGLConfig = *mut c_void;
pub(crate) type EGLContext = *mut c_void;
pub(crate) type EGLSurface = *mut c_void;
type EGLint = i32;

const EGL_NONE: EGLint = 0x3038;
const EGL_PLATFORM_ANGLE_ANGLE: u32 = 0x3202;
const EGL_PLATFORM_ANGLE_TYPE_ANGLE: EGLint = 0x3203;
// The ANGLE back end is Chrome's default for each OS (ui/gl/gl_switches.cc): GLES over the native EGL
// driver on a Linux desktop, Metal on macOS, D3D11 on Windows. Each type's extra platform attributes
// (the Linux GLES path alone wants a device type and Mesa's surfaceless platform) are used only on
// that OS, so the constants are cfg-gated to keep the warnings gate clean.
#[cfg(target_os = "linux")]
const EGL_PLATFORM_ANGLE_TYPE_OPENGLES_ANGLE: EGLint = 0x320E;
#[cfg(target_os = "windows")]
const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: EGLint = 0x3208;
#[cfg(target_os = "macos")]
const EGL_PLATFORM_ANGLE_TYPE_METAL_ANGLE: EGLint = 0x3489;
#[cfg(target_os = "linux")]
const EGL_PLATFORM_ANGLE_DEVICE_TYPE_ANGLE: EGLint = 0x3209;
#[cfg(target_os = "linux")]
const EGL_PLATFORM_ANGLE_DEVICE_TYPE_EGL_ANGLE: EGLint = 0x348E;
#[cfg(target_os = "linux")]
const EGL_PLATFORM_ANGLE_NATIVE_PLATFORM_TYPE_ANGLE: EGLint = 0x348F;
#[cfg(target_os = "linux")]
const EGL_PLATFORM_SURFACELESS_MESA: EGLint = 0x31DD;

// ANGLE's two libraries, named as each OS links them (the loader resolves libGLESv2 first; libEGL
// reads its entry points from it). `scripts/angle-source.sh` builds them per OS beside the binary.
#[cfg(target_os = "linux")]
const ANGLE_GLES: &str = "libGLESv2.so";
#[cfg(target_os = "linux")]
const ANGLE_EGL: &str = "libEGL.so";
#[cfg(target_os = "macos")]
const ANGLE_GLES: &str = "libGLESv2.dylib";
#[cfg(target_os = "macos")]
const ANGLE_EGL: &str = "libEGL.dylib";
#[cfg(target_os = "windows")]
const ANGLE_GLES: &str = "libGLESv2.dll";
#[cfg(target_os = "windows")]
const ANGLE_EGL: &str = "libEGL.dll";
const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_PBUFFER_BIT: EGLint = 0x0001;
const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
const EGL_RED_SIZE: EGLint = 0x3024;
const EGL_GREEN_SIZE: EGLint = 0x3023;
const EGL_BLUE_SIZE: EGLint = 0x3022;
const EGL_ALPHA_SIZE: EGLint = 0x3021;
const EGL_WIDTH: EGLint = 0x3057;
const EGL_HEIGHT: EGLint = 0x3056;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
const EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT: EGLint = 0x30BF;
const EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT: EGLint = 0x3138;
const EGL_LOSE_CONTEXT_ON_RESET_EXT: EGLint = 0x31BF;
const EGL_CONTEXT_BIND_GENERATES_RESOURCE_CHROMIUM: EGLint = 0x33AD;
const EGL_CONTEXT_WEBGL_COMPATIBILITY_ANGLE: EGLint = 0x33AC;
const EGL_EXTENSIONS_ENABLED_ANGLE: EGLint = 0x345F;
const EGL_CONTEXT_CLIENT_ARRAYS_ENABLED_ANGLE: EGLint = 0x3452;
const EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE: EGLint = 0x3453;
const EGL_TRUE: EGLint = 1;
const EGL_FALSE: EGLint = 0;

type GetProcAddress = unsafe extern "C" fn(*const i8) -> *const c_void;
type GetPlatformDisplay = unsafe extern "C" fn(u32, *mut c_void, *const EGLint) -> EGLDisplay;
type Initialize = unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> u32;
type ChooseConfig = unsafe extern "C" fn(EGLDisplay, *const EGLint, *mut EGLConfig, EGLint, *mut EGLint) -> u32;
type BindApi = unsafe extern "C" fn(u32) -> u32;
type CreateContext = unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext;
type DestroyContext = unsafe extern "C" fn(EGLDisplay, EGLContext) -> u32;
type CreatePbufferSurface = unsafe extern "C" fn(EGLDisplay, EGLConfig, *const EGLint) -> EGLSurface;
type DestroySurface = unsafe extern "C" fn(EGLDisplay, EGLSurface) -> u32;
type MakeCurrent = unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> u32;

/// What a WebGL page can read of the device (`WEBGL_debug_renderer_info` and friends), as ANGLE answers it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    /// `GL_VENDOR` of a context, e.g. `Google Inc. (NVIDIA Corporation)` -- `UNMASKED_VENDOR_WEBGL`.
    pub vendor: String,
    /// `GL_RENDERER`, e.g. `ANGLE (NVIDIA Corporation, NVIDIA GeForce RTX 4060/PCIe/SSE2, OpenGL ES 3.2)`.
    pub renderer: String,
    /// `GL_VERSION` of an ES3 context, e.g. `OpenGL ES 3.0 (ANGLE 2.1.27949 git hash: bbf3d8a47552)`.
    pub version: String,
}

/// The device: Chrome's ANGLE, initialized once on the GPU thread, and the config every context is made with.
pub struct Device {
    _gles: libloading::Library,
    _egl: libloading::Library,
    pub(crate) display: EGLDisplay,
    config: EGLConfig,
    get_proc: GetProcAddress,
    create_context: CreateContext,
    destroy_context: DestroyContext,
    create_pbuffer: CreatePbufferSurface,
    destroy_surface: DestroySurface,
    make_current: MakeCurrent,
    pub info: DeviceInfo,
}

/// One WebGL context's GLES context and the 1x1 pbuffer that makes it current.
pub struct DeviceContext {
    pub(crate) context: EGLContext,
    pub(crate) surface: EGLSurface,
}

/// The glvnd EGL vendor library of the GPU the display is on: the DRM card with a connected connector, else the
/// boot VGA card. `None` when there is no such card (a server, a VM without one).
fn display_vendor_library() -> Option<&'static str> {
    let mut boot: Option<String> = None;
    let mut connected: Option<String> = None;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = entry.path().join("device");
        let vendor = std::fs::read_to_string(dev.join("vendor")).unwrap_or_default().trim().to_string();
        if std::fs::read_to_string(dev.join("boot_vga")).is_ok_and(|b| b.trim() == "1") {
            boot = Some(vendor.clone());
        }
        let lit = std::fs::read_dir("/sys/class/drm").ok()?.flatten().any(|c| {
            let n = c.file_name().to_string_lossy().into_owned();
            n.starts_with(&format!("{name}-")) && std::fs::read_to_string(c.path().join("status")).is_ok_and(|s| s.trim() == "connected")
        });
        if lit && connected.is_none() {
            connected = Some(vendor);
        }
    }
    let vendor = connected.or(boot)?;
    Some(if vendor == "0x10de" { "/usr/share/glvnd/egl_vendor.d/10_nvidia.json" } else { "/usr/share/glvnd/egl_vendor.d/50_mesa.json" })
}

/// Where ANGLE's two libraries are: `KORVUS_ANGLE_DIR`; else beside the running executable (a release ships them
/// there, `tools/lean/dist.sh`); else the build `scripts/angle-source.sh` made for the Chrome release this korvus is
/// built for, `${KORVUS_ANGLE_CACHE:-~/.cache/korvus}/angle-<release>`. Per release, so two checkouts on two releases
/// (master and an update worktree) never load each other's ANGLE.
fn angle_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("KORVUS_ANGLE_DIR") {
        return Some(dir.into());
    }
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.to_path_buf()));
    if let Some(dir) = beside.filter(|d| d.join(ANGLE_GLES).exists()) {
        return Some(dir);
    }
    let release = include_str!("../../../../tools/version/chrome.toml")
        .lines()
        .find_map(|l| l.strip_prefix("chrome = \"").and_then(|r| r.strip_suffix('"')))?;
    let cache = std::env::var_os("KORVUS_ANGLE_CACHE").map(std::path::PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache/korvus"))
    })?;
    Some(cache.join(format!("angle-{release}")))
}

/// A renderer string of a CPU rasterizer: Chrome 150 on Linux gives such a device no WebGL.
fn is_software(renderer: &str) -> bool {
    let r = renderer.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "swiftshader", "software rasterizer"].iter().any(|s| r.contains(s))
}

impl Device {
    /// Open the device -- on the thread that will own every context of it. `None`: no usable GPU (see the module).
    pub fn open() -> Option<Device> {
        if let Some(lib) = display_vendor_library() {
            if std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").is_none() && std::path::Path::new(lib).exists() {
                // SAFETY: called on the GPU thread before EGL (glvnd) is loaded; nothing else reads it concurrently.
                unsafe { std::env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", lib) };
            }
        }
        let dir = angle_dir()?.display().to_string();
        // libGLESv2 first: ANGLE's libEGL resolves its entry points from it.
        let gles = unsafe { libloading::Library::new(format!("{dir}/{ANGLE_GLES}")) }.ok()?;
        let egl = unsafe { libloading::Library::new(format!("{dir}/{ANGLE_EGL}")) }.ok()?;
        unsafe {
            let get_proc: GetProcAddress = *egl.get(b"eglGetProcAddress\0").ok()?;
            let gpd = get_proc(c"eglGetPlatformDisplayEXT".as_ptr());
            if gpd.is_null() {
                return None;
            }
            let gpd: GetPlatformDisplay = std::mem::transmute::<*const c_void, GetPlatformDisplay>(gpd);
            let initialize: Initialize = *egl.get(b"eglInitialize\0").ok()?;
            let choose: ChooseConfig = *egl.get(b"eglChooseConfig\0").ok()?;
            let bind_api: BindApi = *egl.get(b"eglBindAPI\0").ok()?;
            #[cfg(target_os = "linux")]
            let attrs = [
                EGL_PLATFORM_ANGLE_TYPE_ANGLE,
                EGL_PLATFORM_ANGLE_TYPE_OPENGLES_ANGLE,
                EGL_PLATFORM_ANGLE_DEVICE_TYPE_ANGLE,
                EGL_PLATFORM_ANGLE_DEVICE_TYPE_EGL_ANGLE,
                EGL_PLATFORM_ANGLE_NATIVE_PLATFORM_TYPE_ANGLE,
                EGL_PLATFORM_SURFACELESS_MESA,
                EGL_NONE,
            ];
            // macOS: ANGLE's Metal back end; Windows: D3D11 -- Chrome's default for each (gl_switches.cc).
            // The type is the only platform attribute they need: ANGLE picks the device itself, and there
            // is no Mesa surfaceless platform or native EGL device type off Linux.
            #[cfg(target_os = "macos")]
            let attrs = [EGL_PLATFORM_ANGLE_TYPE_ANGLE, EGL_PLATFORM_ANGLE_TYPE_METAL_ANGLE, EGL_NONE];
            #[cfg(target_os = "windows")]
            let attrs = [EGL_PLATFORM_ANGLE_TYPE_ANGLE, EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE, EGL_NONE];
            let display = gpd(EGL_PLATFORM_ANGLE_ANGLE, std::ptr::null_mut(), attrs.as_ptr());
            if display.is_null() || initialize(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                return None;
            }
            let config_attrs = [
                EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
                EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE,
            ];
            let mut config: EGLConfig = std::ptr::null_mut();
            let mut n = 0;
            if choose(display, config_attrs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
                return None;
            }
            bind_api(EGL_OPENGL_ES_API);
            let mut device = Device {
                create_context: *egl.get(b"eglCreateContext\0").ok()?,
                destroy_context: *egl.get(b"eglDestroyContext\0").ok()?,
                create_pbuffer: *egl.get(b"eglCreatePbufferSurface\0").ok()?,
                destroy_surface: *egl.get(b"eglDestroySurface\0").ok()?,
                make_current: *egl.get(b"eglMakeCurrent\0").ok()?,
                get_proc,
                _gles: gles,
                _egl: egl,
                display,
                config,
                info: DeviceInfo { vendor: String::new(), renderer: String::new(), version: String::new() },
            };
            // the strings a page reads, from a context as Chrome's WebGL 2 context is made
            let probe = device.create_context(3)?;
            device.make_current(&probe);
            let gl = super::gles::Gles::load(&device);
            let s = |name: u32| {
                let p = (gl.glGetString)(name);
                if p.is_null() { String::new() } else { CStr::from_ptr(p as *const i8).to_string_lossy().into_owned() }
            };
            device.info = DeviceInfo { vendor: s(0x1F00), renderer: s(0x1F01), version: s(0x1F02) };
            device.destroy(probe);
            if is_software(&device.info.renderer) {
                return None;
            }
            Some(device)
        }
    }

    /// A GL entry point of the device.
    pub fn proc_address(&self, name: &str) -> *const c_void {
        let c = CString::new(name).unwrap_or_default();
        // SAFETY: eglGetProcAddress with a NUL-terminated name.
        unsafe { (self.get_proc)(c.as_ptr()) }
    }

    /// A context as Chrome's GPU process makes a WebGL one (`gpu/command_buffer/service/service_utils.cc`
    /// `GenerateGLContextAttribsForDecoder`, passthrough decoder; `ui/gl/gl_context_egl.cc`): ES 2.0 for WebGL 1 and
    /// 3.0 for WebGL 2, WebGL compatibility (extensions off until requested), robust buffer access and resource
    /// initialization, lose context on reset, no bind-generates-resource, no client arrays.
    pub fn create_context(&self, es_major: i32) -> Option<DeviceContext> {
        let attrs = [
            EGL_CONTEXT_MAJOR_VERSION, es_major,
            EGL_CONTEXT_MINOR_VERSION, 0,
            EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT, EGL_TRUE,
            EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT, EGL_LOSE_CONTEXT_ON_RESET_EXT,
            EGL_CONTEXT_BIND_GENERATES_RESOURCE_CHROMIUM, EGL_FALSE,
            EGL_CONTEXT_WEBGL_COMPATIBILITY_ANGLE, EGL_TRUE,
            EGL_EXTENSIONS_ENABLED_ANGLE, EGL_FALSE,
            EGL_CONTEXT_CLIENT_ARRAYS_ENABLED_ANGLE, EGL_FALSE,
            EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE, EGL_TRUE,
            EGL_NONE,
        ];
        // SAFETY: the display and config are this device's; the attribute list is EGL_NONE-terminated.
        unsafe {
            let context = (self.create_context)(self.display, self.config, std::ptr::null_mut(), attrs.as_ptr());
            if context.is_null() {
                return None;
            }
            let pb = [EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE];
            let surface = (self.create_pbuffer)(self.display, self.config, pb.as_ptr());
            if surface.is_null() {
                (self.destroy_context)(self.display, context);
                return None;
            }
            Some(DeviceContext { context, surface })
        }
    }

    pub fn make_current(&self, c: &DeviceContext) -> bool {
        // SAFETY: a context and surface this device made, on the thread that owns the device.
        unsafe { (self.make_current)(self.display, c.surface, c.surface, c.context) != 0 }
    }

    pub fn destroy(&self, c: DeviceContext) {
        // SAFETY: as `make_current`; the context is released before it is destroyed.
        unsafe {
            (self.make_current)(self.display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            (self.destroy_surface)(self.display, c.surface);
            (self.destroy_context)(self.display, c.context);
        }
    }
}
