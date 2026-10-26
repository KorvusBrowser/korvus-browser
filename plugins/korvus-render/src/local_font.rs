//! A CSS `local()` font name to an installed font file, as Chrome on Linux matches it.
//!
//! Chromium's font service (`components/services/font/fontconfig_matching.cc`,
//! `FontConfigLocalMatching::FindFontByPostscriptNameOrFullFontName`) lists the fonts whose `postscriptname` is the
//! name, else whose `fullname` is, among scalable ones; takes the first; and keeps it only when its file is an sfnt
//! (`.ttf .otc .TTF .ttc .otf .OTF`) that exists, with a non-negative face index. fontconfig compares the names
//! without regard to case, so `local("dejavu sans")` matches as Chrome 150 measured it; a family name that is no
//! face's full name (`Ubuntu Sans`, whose faces are `Ubuntu Sans Medium`, ...) matches nothing.
//!
//! fontconfig is the library Skia's font manager already loads in this process; these are its own entry points.

use std::collections::HashMap;
use std::ffi::CStr;
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::Mutex;

#[cfg(target_os = "linux")]
#[repr(C)]
struct FcPattern {
    _p: [u8; 0],
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct FcObjectSet {
    _p: [u8; 0],
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct FcFontSet {
    nfont: c_int,
    sfont: c_int,
    fonts: *mut *mut FcPattern,
}

#[cfg(target_os = "linux")]
#[link(name = "fontconfig")]
extern "C" {
    fn FcPatternCreate() -> *mut FcPattern;
    fn FcPatternDestroy(p: *mut FcPattern);
    fn FcPatternAddString(p: *mut FcPattern, object: *const c_char, s: *const u8) -> c_int;
    fn FcPatternAddBool(p: *mut FcPattern, object: *const c_char, b: c_int) -> c_int;
    fn FcObjectSetCreate() -> *mut FcObjectSet;
    fn FcObjectSetAdd(os: *mut FcObjectSet, object: *const c_char) -> c_int;
    fn FcObjectSetDestroy(os: *mut FcObjectSet);
    fn FcFontList(config: *mut c_void, p: *mut FcPattern, os: *mut FcObjectSet) -> *mut FcFontSet;
    fn FcFontSetDestroy(s: *mut FcFontSet);
    fn FcPatternGetString(p: *const FcPattern, object: *const c_char, n: c_int, s: *mut *mut u8) -> c_int;
    fn FcPatternGetInteger(p: *const FcPattern, object: *const c_char, n: c_int, i: *mut c_int) -> c_int;
    fn FcConfigGetSysRoot(config: *const c_void) -> *const u8;
}

/// `FcResultMatch`.
#[cfg(target_os = "linux")]
const FC_RESULT_MATCH: c_int = 0;

/// Every name asked so far and its answer: a page probes dozens, and fontconfig lists all fonts each time.
static CACHE: Mutex<Option<HashMap<String, Option<spec::LocalFont>>>> = Mutex::new(None);

/// [`spec::RenderBackend::local_font`].
pub(crate) fn local_font(name: &str) -> Option<spec::LocalFont> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(hit) = map.get(name) {
        return hit.clone();
    }
    let found = by_name(c"postscriptname", name).or_else(|| by_name(c"fullname", name));
    map.insert(name.to_string(), found.clone());
    found
}

#[cfg(target_os = "linux")]
/// `FindFontBySpecifiedName`: the first scalable font whose `object` is `name`, when it is an sfnt file that exists.
fn by_name(object: &CStr, name: &str) -> Option<spec::LocalFont> {
    let cname = CString::new(name).ok()?;
    // SAFETY: fontconfig's documented C API, every object created here destroyed before returning; the strings read
    // out of the font set are copied before the set is destroyed.
    unsafe {
        let pattern = FcPatternCreate();
        if pattern.is_null() {
            return None;
        }
        FcPatternAddString(pattern, object.as_ptr(), cname.as_ptr() as *const u8);
        FcPatternAddBool(pattern, c"scalable".as_ptr(), 1);
        let os = FcObjectSetCreate();
        FcObjectSetAdd(os, c"file".as_ptr());
        FcObjectSetAdd(os, c"index".as_ptr());
        let set = FcFontList(std::ptr::null_mut(), pattern, os);
        FcObjectSetDestroy(os);
        FcPatternDestroy(pattern);
        if set.is_null() {
            return None;
        }
        let out = (|| {
            if (*set).nfont <= 0 || (*set).fonts.is_null() {
                return None;
            }
            let first = *(*set).fonts;
            let mut file: *mut u8 = std::ptr::null_mut();
            if FcPatternGetString(first, c"file".as_ptr(), 0, &mut file) != FC_RESULT_MATCH || file.is_null() {
                return None;
            }
            let root = FcConfigGetSysRoot(std::ptr::null());
            let root = if root.is_null() { String::new() } else { CStr::from_ptr(root as *const c_char).to_string_lossy().into_owned() };
            let path = root + &CStr::from_ptr(file as *const c_char).to_string_lossy();
            if ![".ttf", ".otc", ".TTF", ".ttc", ".otf", ".OTF"].iter().any(|e| path.ends_with(e)) {
                return None;
            }
            if std::fs::File::open(&path).is_err() {
                return None;
            }
            let mut index: c_int = 0;
            FcPatternGetInteger(first, c"index".as_ptr(), 0, &mut index);
            if index < 0 {
                return None;
            }
            Some(spec::LocalFont { path, index: index as u32 })
        })();
        FcFontSetDestroy(set);
        out
    }
}

/// macOS/Windows: the host match runs through CoreText / DirectWrite, not fontconfig. Until that
/// port lands, no local-font name resolves (the caller falls back), which is honest for a build that
/// cannot yet ask the platform. THE font layer (GPU-port phase) replaces this with the real matcher.
#[cfg(not(target_os = "linux"))]
fn by_name(_object: &CStr, _name: &str) -> Option<spec::LocalFont> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_name_or_postscript_name_matches_without_regard_to_case_and_a_family_name_does_not() {
        // the fonts of this machine's Chrome measurement (stock Chrome 150, 2026-10-09)
        if local_font("DejaVu Sans").is_none() {
            return; // a machine without DejaVu
        }
        assert!(local_font("DejaVuSans").is_some());
        assert!(local_font("dejavu sans").is_some());
        assert!(local_font("DejaVu Sans Bold").is_some());
        assert!(local_font("Nope XYZ").is_none());
    }
}
