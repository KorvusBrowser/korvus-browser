//! The GLES entry points the executor calls, resolved through the device's `eglGetProcAddress` (ANGLE's).

use std::ffi::c_void;

type E = u32;
type I = i32;
type S = i32;
type F = f32;
type B = u8;

macro_rules! gles {
    ($($name:ident : fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// GLES 2.0/3.0 + the ANGLE extensions the executor uses, as function pointers.
        #[allow(non_snake_case)]
        pub struct Gles { $(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Gles {
            /// Every entry point; a missing one is a missing function of ANGLE (a programming error), so it panics.
            pub fn load(device: &super::device::Device) -> Gles {
                Gles { $($name: {
                    let p = device.proc_address(stringify!($name));
                    assert!(!p.is_null(), concat!("ANGLE has no ", stringify!($name)));
                    // SAFETY: the address of the named GLES entry point, whose signature is the declared one.
                    unsafe { std::mem::transmute::<*const c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(p) }
                },)* }
            }
        }
    };
}

gles! {
    glGetString: fn(E) -> *const u8;
    glGetStringi: fn(E, u32) -> *const u8;
    glGetError: fn() -> E;
    glGetIntegerv: fn(E, *mut I);
    glGetInteger64v: fn(E, *mut i64);
    glGetInternalformativ: fn(E, E, E, S, *mut I);
    glGetShaderPrecisionFormat: fn(E, E, *mut I, *mut I);
    glGetFloatv: fn(E, *mut F);
    glGetBooleanv: fn(E, *mut B);
    glRequestExtensionANGLE: fn(*const i8);
    glCreateShader: fn(E) -> u32;
    glDeleteShader: fn(u32);
    glShaderSource: fn(u32, S, *const *const i8, *const I);
    glCompileShader: fn(u32);
    glGetShaderiv: fn(u32, E, *mut I);
    glGetShaderInfoLog: fn(u32, S, *mut S, *mut i8);
    glCreateProgram: fn() -> u32;
    glDeleteProgram: fn(u32);
    glAttachShader: fn(u32, u32);
    glDetachShader: fn(u32, u32);
    glBindAttribLocation: fn(u32, u32, *const i8);
    glLinkProgram: fn(u32);
    glValidateProgram: fn(u32);
    glUseProgram: fn(u32);
    glGetProgramiv: fn(u32, E, *mut I);
    glGetProgramInfoLog: fn(u32, S, *mut S, *mut i8);
    glGetAttribLocation: fn(u32, *const i8) -> I;
    glGetUniformLocation: fn(u32, *const i8) -> I;
    glGetActiveAttrib: fn(u32, u32, S, *mut S, *mut I, *mut E, *mut i8);
    glGetActiveUniform: fn(u32, u32, S, *mut S, *mut I, *mut E, *mut i8);
    glGetUniformfv: fn(u32, I, *mut F);
    glGetUniformiv: fn(u32, I, *mut I);
    glGetUniformuiv: fn(u32, I, *mut u32);
    glGetUniformBlockIndex: fn(u32, *const i8) -> u32;
    glGetActiveUniformBlockName: fn(u32, u32, S, *mut S, *mut i8);
    glUniformBlockBinding: fn(u32, u32, u32);
    glGetActiveUniformBlockiv: fn(u32, u32, E, *mut I);
    glGetUniformIndices: fn(u32, S, *const *const i8, *mut u32);
    glGetActiveUniformsiv: fn(u32, S, *const u32, E, *mut I);
    glGetFragDataLocation: fn(u32, *const i8) -> I;
    glGetTransformFeedbackVarying: fn(u32, u32, S, *mut S, *mut S, *mut E, *mut i8);
    glTransformFeedbackVaryings: fn(u32, S, *const *const i8, E);
    glGenBuffers: fn(S, *mut u32);
    glDeleteBuffers: fn(S, *const u32);
    glBindBuffer: fn(E, u32);
    glBufferData: fn(E, isize, *const c_void, E);
    glBufferSubData: fn(E, isize, isize, *const c_void);
    glGetBufferParameteriv: fn(E, E, *mut I);
    glGenTextures: fn(S, *mut u32);
    glDeleteTextures: fn(S, *const u32);
    glActiveTexture: fn(E);
    glBindTexture: fn(E, u32);
    glTexImage2D: fn(E, I, I, S, S, I, E, E, *const c_void);
    glTexSubImage2D: fn(E, I, I, I, S, S, E, E, *const c_void);
    glTexStorage2D: fn(E, S, E, S, S);
    glTexStorage3D: fn(E, S, E, S, S, S);
    glTexParameterf: fn(E, E, F);
    glGetTexParameterfv: fn(E, E, *mut F);
    glGetTexParameteriv: fn(E, E, *mut I);
    glGenerateMipmap: fn(E);
    glPixelStorei: fn(E, I);
    glGenFramebuffers: fn(S, *mut u32);
    glDeleteFramebuffers: fn(S, *const u32);
    glBindFramebuffer: fn(E, u32);
    glFramebufferTexture2D: fn(E, E, E, u32, I);
    glFramebufferRenderbuffer: fn(E, E, E, u32);
    glCheckFramebufferStatus: fn(E) -> E;
    glGetFramebufferAttachmentParameteriv: fn(E, E, E, *mut I);
    glGenRenderbuffers: fn(S, *mut u32);
    glDeleteRenderbuffers: fn(S, *const u32);
    glBindRenderbuffer: fn(E, u32);
    glRenderbufferStorage: fn(E, E, S, S);
    glRenderbufferStorageMultisampleANGLE: fn(E, S, E, S, S);
    glGetRenderbufferParameteriv: fn(E, E, *mut I);
    glBlitFramebufferANGLE: fn(I, I, I, I, I, I, I, I, u32, E);
    glGenVertexArraysOES: fn(S, *mut u32);
    glDeleteVertexArraysOES: fn(S, *const u32);
    glBindVertexArrayOES: fn(u32);
    glEnableVertexAttribArray: fn(u32);
    glDisableVertexAttribArray: fn(u32);
    glVertexAttribPointer: fn(u32, I, E, B, S, *const c_void);
    glVertexAttrib4f: fn(u32, F, F, F, F);
    glVertexAttribDivisorANGLE: fn(u32, u32);
    glGetVertexAttribfv: fn(u32, E, *mut F);
    glGetVertexAttribiv: fn(u32, E, *mut I);
    glGetVertexAttribPointerv: fn(u32, E, *mut *mut c_void);
    glUniform1fv: fn(I, S, *const F);
    glUniform2fv: fn(I, S, *const F);
    glUniform3fv: fn(I, S, *const F);
    glUniform4fv: fn(I, S, *const F);
    glUniform1iv: fn(I, S, *const I);
    glUniform2iv: fn(I, S, *const I);
    glUniform3iv: fn(I, S, *const I);
    glUniform4iv: fn(I, S, *const I);
    glUniform1uiv: fn(I, S, *const u32);
    glUniform2uiv: fn(I, S, *const u32);
    glUniform3uiv: fn(I, S, *const u32);
    glUniform4uiv: fn(I, S, *const u32);
    glUniformMatrix2fv: fn(I, S, B, *const F);
    glUniformMatrix3fv: fn(I, S, B, *const F);
    glUniformMatrix4fv: fn(I, S, B, *const F);
    glUniformMatrix2x3fv: fn(I, S, B, *const F);
    glUniformMatrix3x2fv: fn(I, S, B, *const F);
    glUniformMatrix2x4fv: fn(I, S, B, *const F);
    glUniformMatrix4x2fv: fn(I, S, B, *const F);
    glUniformMatrix3x4fv: fn(I, S, B, *const F);
    glUniformMatrix4x3fv: fn(I, S, B, *const F);
    glViewport: fn(I, I, S, S);
    glScissor: fn(I, I, S, S);
    glClearColor: fn(F, F, F, F);
    glClearDepthf: fn(F);
    glClearStencil: fn(I);
    glClear: fn(u32);
    glColorMask: fn(B, B, B, B);
    glDepthMask: fn(B);
    glDepthFunc: fn(E);
    glDepthRangef: fn(F, F);
    glStencilMask: fn(u32);
    glStencilFuncSeparate: fn(E, E, I, u32);
    glStencilOpSeparate: fn(E, E, E, E);
    glEnable: fn(E);
    glDisable: fn(E);
    glIsEnabled: fn(E) -> B;
    glBlendFuncSeparate: fn(E, E, E, E);
    glBlendEquationSeparate: fn(E, E);
    glBlendColor: fn(F, F, F, F);
    glCullFace: fn(E);
    glFrontFace: fn(E);
    glLineWidth: fn(F);
    glPolygonOffset: fn(F, F);
    glSampleCoverage: fn(F, B);
    glHint: fn(E, E);
    glDrawArrays: fn(E, I, S);
    glDrawElements: fn(E, S, E, *const c_void);
    glDrawArraysInstancedANGLE: fn(E, I, S, S);
    glDrawElementsInstancedANGLE: fn(E, S, E, *const c_void, S);
    glFinish: fn();
    glFlush: fn();
    glReadPixels: fn(I, I, S, S, E, E, *mut c_void);
}
