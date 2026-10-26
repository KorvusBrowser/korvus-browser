//! Pipeline tests. Every one of them asserts on PIXELS or on a value the pipeline computed — none
//! asserts that an op was recorded, because a recorded op that nothing executes is the exact failure
//! this module was written to remove.

use super::*;

const VS: &str = "attribute vec2 a_pos; void main() { gl_Position = vec4(a_pos, 0.0, 1.0); }";
const FS_RED: &str = "precision mediump float; void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }";

fn attrs() -> GlContextAttributes {
    // Antialiasing off in the pipeline tests: a multisampled edge is the right default for a page
    // and the wrong one for an assertion about an exact texel.
    GlContextAttributes { antialias: false, ..Default::default() }
}

fn px(img: &Rgba8, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    [img.data[i], img.data[i + 1], img.data[i + 2], img.data[i + 3]]
}

/// A program that draws a full-viewport quad, ready to have a fragment shader plugged in.
fn quad_program(fs: &str) -> Vec<GlOp> {
    let verts: Vec<f32> = vec![-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0];
    let mut bytes = Vec::new();
    for v in &verts {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    vec![
        GlOp::CreateShader { id: 1, kind: GlShaderKind::Vertex },
        GlOp::ShaderSource { shader: 1, source: VS.into() },
        GlOp::CompileShader { shader: 1 },
        GlOp::CreateShader { id: 2, kind: GlShaderKind::Fragment },
        GlOp::ShaderSource { shader: 2, source: fs.into() },
        GlOp::CompileShader { shader: 2 },
        GlOp::CreateProgram { id: 3 },
        GlOp::AttachShader { program: 3, shader: 1 },
        GlOp::AttachShader { program: 3, shader: 2 },
        GlOp::LinkProgram { program: 3 },
        GlOp::UseProgram { program: 3 },
        GlOp::CreateBuffer { id: 4 },
        GlOp::BindBuffer { target: ARRAY_BUFFER, buffer: 4 },
        GlOp::BufferData { target: ARRAY_BUFFER, size: bytes.len(), data: bytes, usage: 0x88E4 },
        GlOp::EnableVertexAttribArray { index: 0 },
        GlOp::VertexAttribPointer { index: 0, size: 2, ty: FLOAT, normalized: false, stride: 0, offset: 0 },
    ]
}

#[test]
fn clear_color_reaches_the_pixels_and_two_colours_differ() {
    // The discriminator this whole unit exists for: at HEAD these two produced byte-identical
    // 174-byte data URLs.
    let a = raster(
        &[GlOp::ClearColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }, GlOp::Clear { mask: COLOR_BUFFER_BIT }],
        4,
        4,
        &attrs(),
    );
    let b = raster(
        &[GlOp::ClearColor { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }, GlOp::Clear { mask: COLOR_BUFFER_BIT }],
        4,
        4,
        &attrs(),
    );
    assert_eq!(px(&a, 2, 2), [255, 0, 0, 255]);
    assert_eq!(px(&b, 2, 2), [0, 0, 255, 255]);
    assert_ne!(a.data, b.data);
}

#[test]
fn an_unclear_canvas_is_transparent_black() {
    let img = raster(&[], 4, 4, &attrs());
    assert!(img.data.iter().all(|&b| b == 0));
}

#[test]
fn a_shaded_quad_paints_what_the_fragment_shader_returns() {
    let mut ops = quad_program(FS_RED);
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    assert_eq!(px(&img, 4, 4), [255, 0, 0, 255], "the quad is red");
}

#[test]
fn the_pixels_depend_on_the_program_not_just_on_the_ops() {
    // Same op list, one differing character in the fragment shader.
    let draw = |fs: &str| {
        let mut ops = quad_program(fs);
        ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
        raster(&ops, 8, 8, &attrs())
    };
    let a = draw("precision mediump float; void main() { gl_FragColor = vec4(0.25, 0.5, 0.75, 1.0); }");
    let b = draw("precision mediump float; void main() { gl_FragColor = vec4(0.75, 0.5, 0.25, 1.0); }");
    // 0.5 * 255 is a tie, stored DOWN as the persona's GPU stores it (see `unorm8`)
    assert_eq!(px(&a, 4, 4), [64, 127, 191, 255]);
    assert_eq!(px(&b, 4, 4), [191, 127, 64, 255]);
}

#[test]
fn a_uniform_changes_the_frame() {
    let fs = "precision mediump float; uniform vec4 u_c; void main() { gl_FragColor = u_c; }";
    let mut ops = quad_program(fs);
    ops.push(GlOp::Uniform {
        program: 3,
        name: "u_c".into(),
        value: GlUniformValue::Float { components: 4, values: vec![0.0, 1.0, 0.0, 1.0] },
    });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    assert_eq!(px(&img, 4, 4), [0, 255, 0, 255]);
}

#[test]
fn a_varying_is_interpolated_across_the_triangle() {
    let vs = "attribute vec2 a_pos; varying vec2 v; void main() { v = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }";
    let fs = "precision mediump float; varying vec2 v; void main() { gl_FragColor = vec4(v, 0.0, 1.0); }";
    let mut ops = quad_program(fs);
    ops[1] = GlOp::ShaderSource { shader: 1, source: vs.into() };
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 16, 16, &attrs());
    // Rgba8 row 0 is the TOP; the gradient's red channel grows left to right and green grows
    // upward, so the top-left corner is dark red / bright green.
    let left = px(&img, 1, 8);
    let right = px(&img, 14, 8);
    assert!(right[0] > left[0] + 100, "red grows to the right: {left:?} -> {right:?}");
    let top = px(&img, 8, 1);
    let bottom = px(&img, 8, 14);
    assert!(top[1] > bottom[1] + 100, "green grows upward: {bottom:?} -> {top:?}");
}

#[test]
fn the_viewport_confines_the_draw() {
    let mut ops = quad_program(FS_RED);
    ops.push(GlOp::Viewport { x: 0, y: 0, width: 4, height: 4 });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    // The viewport's y=0..4 is the BOTTOM half of the drawing buffer, which is the bottom half of
    // the image after the flip.
    assert_eq!(px(&img, 1, 6), [255, 0, 0, 255], "inside the viewport");
    assert_eq!(px(&img, 6, 1), [0, 0, 0, 0], "outside it");
}

#[test]
fn the_scissor_box_confines_a_clear() {
    let ops = vec![
        GlOp::Enable { cap: SCISSOR_TEST },
        GlOp::Scissor { x: 0, y: 0, width: 4, height: 8 },
        GlOp::ClearColor { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
        GlOp::Clear { mask: COLOR_BUFFER_BIT },
    ];
    let img = raster(&ops, 8, 8, &attrs());
    assert_eq!(px(&img, 1, 4), [0, 255, 0, 255]);
    assert_eq!(px(&img, 6, 4), [0, 0, 0, 0]);
}

#[test]
fn blending_composites_over_what_is_already_there() {
    let fs = "precision mediump float; void main() { gl_FragColor = vec4(0.0, 0.0, 1.0, 0.5); }";
    let mut ops = vec![
        GlOp::ClearColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
        GlOp::Clear { mask: COLOR_BUFFER_BIT },
        GlOp::Enable { cap: BLEND },
        GlOp::BlendFuncSeparate { src_rgb: 0x0302, dst_rgb: 0x0303, src_alpha: 0x0302, dst_alpha: 0x0303 },
    ];
    ops.extend(quad_program(fs));
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    let p = px(&img, 4, 4);
    assert!(p[0] > 120 && p[0] < 135, "half the red survives: {p:?}");
    assert!(p[2] > 120 && p[2] < 135, "half the blue arrives: {p:?}");
}

#[test]
fn the_depth_test_hides_the_farther_triangle() {
    // Two full-screen quads: the second is farther and must not overwrite the first.
    let vs_near = "attribute vec2 a_pos; uniform float u_z; void main() { gl_Position = vec4(a_pos, u_z, 1.0); }";
    let fs = "precision mediump float; uniform vec4 u_c; void main() { gl_FragColor = u_c; }";
    let mut ops = quad_program(fs);
    ops[1] = GlOp::ShaderSource { shader: 1, source: vs_near.into() };
    ops.push(GlOp::Enable { cap: DEPTH_TEST });
    ops.push(GlOp::Uniform { program: 3, name: "u_z".into(), value: GlUniformValue::Float { components: 1, values: vec![-0.5] } });
    ops.push(GlOp::Uniform { program: 3, name: "u_c".into(), value: GlUniformValue::Float { components: 4, values: vec![0.0, 1.0, 0.0, 1.0] } });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    ops.push(GlOp::Uniform { program: 3, name: "u_z".into(), value: GlUniformValue::Float { components: 1, values: vec![0.5] } });
    ops.push(GlOp::Uniform { program: 3, name: "u_c".into(), value: GlUniformValue::Float { components: 4, values: vec![1.0, 0.0, 0.0, 1.0] } });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    assert_eq!(px(&img, 4, 4), [0, 255, 0, 255], "the near green quad survives");
}

#[test]
fn back_face_culling_removes_a_clockwise_triangle() {
    let cw: Vec<f32> = vec![-1.0, -1.0, -1.0, 1.0, 1.0, -1.0];
    let mut bytes = Vec::new();
    for v in &cw {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let mut ops = quad_program(FS_RED);
    // Replace the buffer contents with a single clockwise triangle.
    for op in ops.iter_mut() {
        if let GlOp::BufferData { data, size, .. } = op {
            *data = bytes.clone();
            *size = bytes.len();
        }
    }
    ops.push(GlOp::Enable { cap: CULL_FACE });
    ops.push(GlOp::CullFace { mode: BACK });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 3 });
    let img = raster(&ops, 8, 8, &attrs());
    assert!(img.data.iter().all(|&b| b == 0), "the back-facing triangle was culled");
}

#[test]
fn draw_elements_reads_the_index_buffer() {
    let mut ops = quad_program(FS_RED);
    let idx: Vec<u8> = [0u16, 1, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
    ops.push(GlOp::CreateBuffer { id: 5 });
    ops.push(GlOp::BindBuffer { target: ELEMENT_ARRAY_BUFFER, buffer: 5 });
    ops.push(GlOp::BufferData { target: ELEMENT_ARRAY_BUFFER, size: idx.len(), data: idx, usage: 0x88E4 });
    ops.push(GlOp::DrawElements { mode: TRIANGLES, count: 3, ty: UNSIGNED_SHORT, offset: 0 });
    let img = raster(&ops, 8, 8, &attrs());
    // The first three vertices are the lower-left triangle.
    assert_eq!(px(&img, 1, 7), [255, 0, 0, 255]);
}

#[test]
fn a_texture_is_sampled_by_the_fragment_shader() {
    let fs = "precision mediump float; uniform sampler2D t; varying vec2 v;
              void main() { gl_FragColor = texture2D(t, v); }";
    let vs = "attribute vec2 a_pos; varying vec2 v; void main() { v = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }";
    // A 2x2 texture: bottom-left magenta, the rest black.
    let tex = Rgba8 {
        width: 2,
        height: 2,
        data: vec![255, 0, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    };
    let mut ops = quad_program(fs);
    ops[1] = GlOp::ShaderSource { shader: 1, source: vs.into() };
    ops.push(GlOp::CreateTexture { id: 9 });
    ops.push(GlOp::ActiveTexture { unit: 0x84C0 });
    ops.push(GlOp::BindTexture { target: TEXTURE_2D, texture: 9 });
    ops.push(GlOp::TexImage2D {
        target: TEXTURE_2D,
        level: 0,
        internal_format: 0x1908,
        width: 2,
        height: 2,
        format: 0x1908,
        ty: UNSIGNED_BYTE,
        pixels: Some(tex),
    });
    ops.push(GlOp::TexParameter { target: TEXTURE_2D, pname: TEXTURE_MIN_FILTER, param: NEAREST as f32 });
    ops.push(GlOp::TexParameter { target: TEXTURE_2D, pname: TEXTURE_MAG_FILTER, param: NEAREST as f32 });
    ops.push(GlOp::Uniform { program: 3, name: "t".into(), value: GlUniformValue::Int { components: 1, values: vec![0] } });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    // t=0 is the texture's first row, which lands at the BOTTOM of the drawing buffer, i.e. the
    // bottom of the flipped image.
    assert_eq!(px(&img, 1, 7), [255, 0, 255, 255], "the magenta texel");
    assert_eq!(px(&img, 6, 1), [0, 0, 0, 255], "a black one");
}

#[test]
fn a_failed_compile_reports_the_reason_and_the_link_fails_with_it() {
    let ops = vec![
        GlOp::CreateShader { id: 1, kind: GlShaderKind::Vertex },
        GlOp::ShaderSource { shader: 1, source: "void main( {".into() },
        GlOp::CompileShader { shader: 1 },
        GlOp::CreateProgram { id: 3 },
        GlOp::AttachShader { program: 3, shader: 1 },
        GlOp::LinkProgram { program: 3 },
    ];
    let a = attrs();
    assert_eq!(
        query(&ops, &GlQuery::ShaderParameter { shader: 1, pname: COMPILE_STATUS }, 1, 1, &a),
        GlQueryResult::Bool(false)
    );
    match query(&ops, &GlQuery::ShaderInfoLog { shader: 1 }, 1, 1, &a) {
        GlQueryResult::Str(s) => assert!(s.starts_with("ERROR:"), "{s}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        query(&ops, &GlQuery::ProgramParameter { program: 3, pname: LINK_STATUS }, 1, 1, &a),
        GlQueryResult::Bool(false)
    );
}

#[test]
fn locations_come_from_the_linked_program() {
    let ops = quad_program(FS_RED);
    let a = attrs();
    assert_eq!(
        query(&ops, &GlQuery::AttribLocation { program: 3, name: "a_pos".into() }, 1, 1, &a),
        GlQueryResult::Int(0)
    );
    assert_eq!(
        query(&ops, &GlQuery::AttribLocation { program: 3, name: "nope".into() }, 1, 1, &a),
        GlQueryResult::Int(-1)
    );
    assert_eq!(
        query(&ops, &GlQuery::UniformLocation { program: 3, name: "nope".into() }, 1, 1, &a),
        GlQueryResult::None
    );
    assert_eq!(
        query(&ops, &GlQuery::ProgramParameter { program: 3, pname: ACTIVE_ATTRIBUTES }, 1, 1, &a),
        GlQueryResult::Int(1)
    );
}

#[test]
fn bind_attrib_location_is_honoured_at_link() {
    let mut ops = quad_program(FS_RED);
    let link_at = ops.iter().position(|o| matches!(o, GlOp::LinkProgram { .. })).unwrap();
    ops.insert(link_at, GlOp::BindAttribLocation { program: 3, index: 3, name: "a_pos".into() });
    assert_eq!(
        query(&ops, &GlQuery::AttribLocation { program: 3, name: "a_pos".into() }, 1, 1, &attrs()),
        GlQueryResult::Int(3)
    );
}

#[test]
fn get_error_reports_a_draw_with_no_program() {
    let ops = vec![GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 3 }];
    assert_eq!(query(&ops, &GlQuery::Error { consumed: 0 }, 8, 8, &attrs()), GlQueryResult::UInt(INVALID_OPERATION));
}

#[test]
fn is_object_answers_about_the_objects_the_list_made() {
    let ops = vec![GlOp::CreateBuffer { id: 7 }];
    let a = attrs();
    assert_eq!(
        query(&ops, &GlQuery::IsObject { kind: GlObjectKind::Buffer, id: 7 }, 1, 1, &a),
        GlQueryResult::Bool(true)
    );
    assert_eq!(
        query(&ops, &GlQuery::IsObject { kind: GlObjectKind::Buffer, id: 8 }, 1, 1, &a),
        GlQueryResult::Bool(false)
    );
}

#[test]
fn a_runaway_fragment_shader_aborts_the_frame_instead_of_hanging() {
    let fs = "precision mediump float; void main() {
                float s = 0.0;
                for (int i = 0; i < 100000000; i++) { s += 0.000001; }
                gl_FragColor = vec4(s, 0.0, 0.0, 1.0);
              }";
    let mut ops = quad_program(fs);
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let mut m = GlMachine::new(16, 16, &attrs());
    m.run(&ops);
    assert!(m.aborted.is_some(), "the frame gave up rather than running forever");
}

#[test]
fn antialias_resolves_a_diagonal_edge_to_intermediate_coverage() {
    // A half-screen triangle with a diagonal edge. With multisampling on, the edge pixels are
    // partly covered; with it off, every pixel is 0 or 255.
    let tri: Vec<f32> = vec![-1.0, -1.0, 1.0, -1.0, -1.0, 1.0];
    let mut bytes = Vec::new();
    for v in &tri {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let build = |aa: bool| {
        let mut ops = quad_program(FS_RED);
        for op in ops.iter_mut() {
            if let GlOp::BufferData { data, size, .. } = op {
                *data = bytes.clone();
                *size = bytes.len();
            }
        }
        ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 3 });
        raster(&ops, 16, 16, &GlContextAttributes { antialias: aa, ..Default::default() })
    };
    let hard = build(false);
    let soft = build(true);
    let partial = |img: &Rgba8| img.data.chunks(4).filter(|p| p[3] > 0 && p[3] < 255).count();
    assert_eq!(partial(&hard), 0, "no antialiasing means no partial coverage");
    assert!(partial(&soft) > 0, "antialiasing resolves the diagonal");
}

#[test]
fn rendering_to_a_framebuffer_texture_and_sampling_it_back() {
    let fs_green = "precision mediump float; void main() { gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0); }";
    let mut ops = quad_program(fs_green);
    // A 4x4 colour texture, attached to an FBO, drawn into.
    ops.push(GlOp::CreateTexture { id: 9 });
    ops.push(GlOp::BindTexture { target: TEXTURE_2D, texture: 9 });
    ops.push(GlOp::TexImage2D {
        target: TEXTURE_2D,
        level: 0,
        internal_format: 0x1908,
        width: 4,
        height: 4,
        format: 0x1908,
        ty: UNSIGNED_BYTE,
        pixels: None,
    });
    ops.push(GlOp::CreateFramebuffer { id: 10 });
    ops.push(GlOp::BindFramebuffer { target: 0x8D40, framebuffer: 10 });
    ops.push(GlOp::FramebufferTexture2D {
        target: 0x8D40,
        attachment: COLOR_ATTACHMENT0,
        textarget: TEXTURE_2D,
        texture: 9,
        level: 0,
    });
    ops.push(GlOp::Viewport { x: 0, y: 0, width: 4, height: 4 });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    // Back to the default framebuffer, sampling the texture we just rendered.
    ops.push(GlOp::BindFramebuffer { target: 0x8D40, framebuffer: 0 });
    ops.push(GlOp::Viewport { x: 0, y: 0, width: 8, height: 8 });
    let sample_fs = "precision mediump float; uniform sampler2D t; void main() { gl_FragColor = texture2D(t, vec2(0.5)); }";
    ops.push(GlOp::CreateShader { id: 11, kind: GlShaderKind::Fragment });
    ops.push(GlOp::ShaderSource { shader: 11, source: sample_fs.into() });
    ops.push(GlOp::CompileShader { shader: 11 });
    ops.push(GlOp::CreateProgram { id: 12 });
    ops.push(GlOp::AttachShader { program: 12, shader: 1 });
    ops.push(GlOp::AttachShader { program: 12, shader: 11 });
    ops.push(GlOp::LinkProgram { program: 12 });
    ops.push(GlOp::UseProgram { program: 12 });
    ops.push(GlOp::TexParameter { target: TEXTURE_2D, pname: TEXTURE_MIN_FILTER, param: NEAREST as f32 });
    ops.push(GlOp::TexParameter { target: TEXTURE_2D, pname: TEXTURE_MAG_FILTER, param: NEAREST as f32 });
    ops.push(GlOp::Uniform { program: 12, name: "t".into(), value: GlUniformValue::Int { components: 1, values: vec![0] } });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    assert_eq!(px(&img, 4, 4), [0, 255, 0, 255], "the render-to-texture round trip");
}

#[test]
fn a_matrix_uniform_transforms_the_geometry() {
    // A scale-by-a-half matrix shrinks a full-screen quad to the middle of the viewport.
    let vs = "attribute vec2 a_pos; uniform mat4 m; void main() { gl_Position = m * vec4(a_pos, 0.0, 1.0); }";
    let mut ops = quad_program(FS_RED);
    ops[1] = GlOp::ShaderSource { shader: 1, source: vs.into() };
    #[rustfmt::skip]
    let m = vec![0.5, 0.0, 0.0, 0.0,
                 0.0, 0.5, 0.0, 0.0,
                 0.0, 0.0, 1.0, 0.0,
                 0.0, 0.0, 0.0, 1.0];
    ops.push(GlOp::Uniform {
        program: 3,
        name: "m".into(),
        value: GlUniformValue::Matrix { rows: 4, cols: 4, transpose: false, values: m },
    });
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 16, 16, &attrs());
    assert_eq!(px(&img, 8, 8), [255, 0, 0, 255], "the middle is covered");
    assert_eq!(px(&img, 1, 1), [0, 0, 0, 0], "the corner is not");
}

#[test]
fn points_are_rasterized_at_their_point_size() {
    let vs = "attribute vec2 a_pos; void main() { gl_PointSize = 5.0; gl_Position = vec4(a_pos, 0.0, 1.0); }";
    let mut ops = quad_program(FS_RED);
    ops[1] = GlOp::ShaderSource { shader: 1, source: vs.into() };
    let one: Vec<u8> = [0.0f32, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    for op in ops.iter_mut() {
        if let GlOp::BufferData { data, size, .. } = op {
            *data = one.clone();
            *size = one.len();
        }
    }
    ops.push(GlOp::DrawArrays { mode: POINTS, first: 0, count: 1 });
    let img = raster(&ops, 16, 16, &attrs());
    assert_eq!(px(&img, 8, 8), [255, 0, 0, 255], "the point covers the centre");
    assert_eq!(px(&img, 0, 0), [0, 0, 0, 0], "and not the corner");
}

#[test]
fn readback_is_the_same_buffer_a_second_run_produces() {
    // The determinism `readPixels`/`toDataURL` agreement rests on: two runs of one list are equal.
    let mut ops = quad_program(FS_RED);
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    assert_eq!(raster(&ops, 8, 8, &attrs()).data, raster(&ops, 8, 8, &attrs()).data);
}

#[test]
fn read_pixels_and_the_encoded_buffer_are_the_same_pixels() {
    // The cross-check a detector runs, and the one the 2D path once failed: the two readback paths
    // must describe one buffer. `readPixels` counts rows from the BOTTOM, `Rgba8` from the top.
    let mut ops = quad_program(
        "precision mediump float; varying vec2 v; void main() { gl_FragColor = vec4(v, 0.0, 1.0); }",
    );
    ops[1] = GlOp::ShaderSource {
        shader: 1,
        source: "attribute vec2 a_pos; varying vec2 v; void main() { v = a_pos * 0.5 + 0.5; gl_Position = vec4(a_pos, 0.0, 1.0); }".into(),
    };
    ops.push(GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 6 });
    let img = raster(&ops, 8, 8, &attrs());
    let bytes = match query(&ops, &GlQuery::ReadPixels { x: 0, y: 0, width: 8, height: 8 }, 8, 8, &attrs()) {
        GlQueryResult::Bytes(b) => b,
        other => panic!("{other:?}"),
    };
    for y in 0..8u32 {
        for x in 0..8u32 {
            let from_read = &bytes[(((y * 8) + x) * 4) as usize..][..4];
            let from_img = px(&img, x, 7 - y);
            assert_eq!(from_read, &from_img[..], "({x},{y})");
        }
    }
}

#[test]
fn get_error_is_read_and_clear() {
    let ops = vec![
        GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 3 },
        GlOp::BindBuffer { target: 0xDEAD, buffer: 0 },
    ];
    let a = attrs();
    assert_eq!(query(&ops, &GlQuery::Error { consumed: 0 }, 4, 4, &a), GlQueryResult::UInt(INVALID_OPERATION));
    assert_eq!(query(&ops, &GlQuery::Error { consumed: 1 }, 4, 4, &a), GlQueryResult::UInt(INVALID_ENUM));
    assert_eq!(query(&ops, &GlQuery::Error { consumed: 2 }, 4, 4, &a), GlQueryResult::UInt(NO_ERROR));
}

/// A question about a named list, answered by the machine kept from the list's last question
/// and run on over what was recorded since, is the question a replay from nothing answers -- over
/// random lists of shader, program, uniform, buffer, attribute, texture and capability ops (draws and
/// clears among them), two lists grown in turns, each now and then started over under a new name.
#[test]
fn a_kept_query_machine_answers_as_a_replay_from_nothing() {
    const SOURCES: [&str; 4] = [
        "attribute vec2 a_pos; uniform vec2 u_off; void main() { gl_Position = vec4(a_pos + u_off, 0.0, 1.0); }",
        "precision mediump float; uniform vec4 u_col; uniform float u_k[2]; void main() { gl_FragColor = u_col * u_k[1]; }",
        "precision mediump float; uniform vec4 u_col; void main() { gl_FragColor = u_col; }",
        "void main( {",
    ];
    let shaders = [1u32, 2, 3, 4];
    let programs = [10u32, 11];
    for seed in 1..=8u64 {
        let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % n as u64) as usize
        };
        // (name, attributes, ops)
        let mut lists = [(1000 + seed * 10, attrs(), Vec::new()), (1001 + seed * 10, GlContextAttributes::default(), Vec::new())];
        let mut fresh_names = 2000 + seed * 100;
        for step in 0..300 {
            let which = next(2);
            if next(60) == 0 {
                fresh_names += 1;
                lists[which].0 = fresh_names;
                lists[which].2.clear();
            }
            let ops = &mut lists[which].2;
            for _ in 0..1 + next(4) {
                let (s, p) = (shaders[next(shaders.len())], programs[next(programs.len())]);
                let op = match next(16) {
                    0 => GlOp::CreateShader { id: s, kind: if s % 2 == 1 { GlShaderKind::Vertex } else { GlShaderKind::Fragment } },
                    1 => GlOp::ShaderSource { shader: s, source: SOURCES[next(SOURCES.len())].into() },
                    2 => GlOp::CompileShader { shader: s },
                    3 => GlOp::CreateProgram { id: p },
                    4 => GlOp::AttachShader { program: p, shader: s },
                    5 => GlOp::LinkProgram { program: p },
                    6 => GlOp::UseProgram { program: p },
                    7 => {
                        let (name, value) = match next(3) {
                            0 => ("u_col", GlUniformValue::Float { components: 4, values: vec![0.5, 0.25, next(9) as f32, 1.0] }),
                            1 => ("u_off", GlUniformValue::Float { components: 2, values: vec![next(5) as f32, 0.0] }),
                            _ => ("u_k", GlUniformValue::Float { components: 1, values: vec![2.0, next(7) as f32] }),
                        };
                        GlOp::Uniform { program: p, name: name.into(), value }
                    }
                    8 => if next(2) == 0 { GlOp::DeleteShader { id: s } } else { GlOp::DeleteProgram { id: p } },
                    9 => GlOp::DetachShader { program: p, shader: s },
                    10 => GlOp::BindAttribLocation { program: p, index: next(3) as u32, name: "a_pos".into() },
                    11 => match next(3) {
                        0 => GlOp::CreateBuffer { id: 20 },
                        1 => GlOp::BindBuffer { target: ARRAY_BUFFER, buffer: 20 },
                        _ => GlOp::BufferData { target: ARRAY_BUFFER, size: 4 * (1 + next(6)), data: vec![0; 4 * (1 + next(6))], usage: 0x88E4 },
                    },
                    12 => match next(3) {
                        0 => GlOp::EnableVertexAttribArray { index: 0 },
                        1 => GlOp::DisableVertexAttribArray { index: 0 },
                        _ => GlOp::VertexAttribPointer { index: 0, size: 2, ty: FLOAT, normalized: false, stride: 0, offset: 0 },
                    },
                    13 => match next(3) {
                        0 => GlOp::CreateTexture { id: 30 },
                        1 => GlOp::BindTexture { target: TEXTURE_2D, texture: 30 },
                        _ => GlOp::TexParameter { target: TEXTURE_2D, pname: TEXTURE_MIN_FILTER, param: [LINEAR, NEAREST][next(2)] as f32 },
                    },
                    14 => [GlOp::Enable { cap: [BLEND, DEPTH_TEST, CULL_FACE][next(3)] }, GlOp::Disable { cap: BLEND }][next(2)].clone(),
                    _ => if next(2) == 0 { GlOp::DrawArrays { mode: TRIANGLES, first: 0, count: 3 } } else { GlOp::Clear { mask: COLOR_BUFFER_BIT } },
                };
                ops.push(op);
            }
            // Questions about either list: the one just grown, and now and then the other.
            for _ in 0..3 {
                let asked = if next(4) == 0 { 1 - which } else { which };
                let (name, a, ops) = &lists[asked];
                let (s, p) = (shaders[next(shaders.len())], programs[next(programs.len())]);
                let q = match next(17) {
                    0 => GlQuery::ShaderParameter { shader: s, pname: [COMPILE_STATUS, DELETE_STATUS, SHADER_TYPE][next(3)] },
                    1 => GlQuery::ShaderInfoLog { shader: s },
                    2 => GlQuery::ShaderSource { shader: s },
                    3 => GlQuery::ProgramParameter { program: p, pname: [LINK_STATUS, ACTIVE_UNIFORMS, ATTACHED_SHADERS, DELETE_STATUS][next(4)] },
                    4 => GlQuery::ProgramInfoLog { program: p },
                    5 => GlQuery::AttachedShaders { program: p },
                    6 => GlQuery::AttribLocation { program: p, name: "a_pos".into() },
                    7 => GlQuery::UniformLocation { program: p, name: ["u_col", "u_k[1]", "u_off", "nope"][next(4)].into() },
                    8 => GlQuery::ActiveUniform { program: p, index: next(3) as u32 },
                    9 => GlQuery::Uniform { program: p, name: ["u_col", "u_k", "u_off"][next(3)].into() },
                    10 => GlQuery::IsObject { kind: [GlObjectKind::Buffer, GlObjectKind::Shader, GlObjectKind::Program][next(3)], id: [20, s, p][next(3)] },
                    11 => GlQuery::IsEnabled { cap: [BLEND, DEPTH_TEST, CULL_FACE][next(3)] },
                    12 => GlQuery::BufferParameter { target: ARRAY_BUFFER, pname: BUFFER_SIZE },
                    13 => GlQuery::VertexAttrib { index: 0, pname: VERTEX_ATTRIB_ARRAY_ENABLED },
                    // the two that DRAW: a kept drawing machine answers them
                    15 => GlQuery::ReadPixels { x: 0, y: 0, width: 8, height: 8 },
                    16 => GlQuery::Error { consumed: next(3) as u32 },
                    _ => GlQuery::TexParameter { target: TEXTURE_2D, pname: TEXTURE_MIN_FILTER },
                };
                assert_eq!(
                    query_list(ops, &q, 8, 8, a, Some(*name)),
                    query(ops, &q, 8, 8, a),
                    "seed {seed}, step {step}, list {name}: {q:?}"
                );
            }
        }
    }
}

/// The R channel of a `readPixels` of the whole buffer, bottom row first: `.` between pixels, `/`
/// between rows -- the shape a probe recorded Chrome's in.
fn red_rows(ops: &[GlOp], w: u32, h: u32) -> String {
    let q = GlQuery::ReadPixels { x: 0, y: 0, width: w, height: h };
    let GlQueryResult::Bytes(px) = query(ops, &q, w, h, &GlContextAttributes::default()) else { panic!("no pixels") };
    (0..h as usize)
        .map(|y| (0..w as usize).map(|x| px[(y * w as usize + x) * 4].to_string()).collect::<Vec<_>>().join("."))
        .collect::<Vec<_>>()
        .join("/")
}

/// `quad_program` with `fs`, drawing `verts` (x, y pairs) as `mode`, into a cleared opaque black buffer.
fn draw_verts(fs: &str, verts: &[f32], mode: u32) -> Vec<GlOp> {
    let mut bytes = Vec::new();
    for v in verts {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    let mut ops = quad_program(fs);
    for op in ops.iter_mut() {
        if let GlOp::BufferData { data, size, .. } = op {
            *data = bytes.clone();
            *size = bytes.len();
        }
    }
    ops.push(GlOp::ClearColor { r: 0.0, g: 0.0, b: 0.0, a: 1.0 });
    ops.push(GlOp::Clear { mask: COLOR_BUFFER_BIT });
    ops.push(GlOp::DrawArrays { mode, first: 0, count: verts.len() as i32 / 2 });
    ops
}

/// a bot-detection script's WebGL proof of work (`captcha.js`): a shader that colours by
/// `mod(gl_FragCoord.x, 2.0)`, one triangle, an antialiased 16x9 buffer read back whole. The rows are
/// stock Chrome 150's on the persona's GPU (ANGLE, RTX 4060):
/// the shader ran once per pixel at its centre (every covered pixel 0.5 -> 127, a tie stored down) and
/// the edges resolved from the coverage of the four standard sample positions (32, 64, 95).
#[test]
fn a_multisampled_triangle_resolves_as_the_gpu_does() {
    let px_fs = "precision lowp float;uniform float v;void main() {vec4 c = vec4(.3, .3, .3, 1);float mx = mod(gl_FragCoord.x, 2.0);float my = mod(gl_FragCoord.y, 2.0);if (mx > 0.) c.x = .5;else c.x = 1.;if (my > 0.) c.z = .5;else c.z = 1.;c.y = c.x - c.z;gl_FragColor = c;}";
    let ops = draw_verts(px_fs, &[-1.0, 1.0, 1.0, 1.0, 0.0, -1.0, -1.0, 1.0], TRIANGLE_STRIP);
    assert_eq!(
        red_rows(&ops, 16, 9),
        "0.0.0.0.0.0.0.64.64.0.0.0.0.0.0.0/0.0.0.0.0.0.64.127.127.64.0.0.0.0.0.0/0.0.0.0.0.32.127.127.127.127.32.0.0.0.0.0/\
         0.0.0.0.32.127.127.127.127.127.127.32.0.0.0.0/0.0.0.0.127.127.127.127.127.127.127.127.0.0.0.0/\
         0.0.0.95.127.127.127.127.127.127.127.127.95.0.0.0/0.0.95.127.127.127.127.127.127.127.127.127.127.95.0.0/\
         0.64.127.127.127.127.127.127.127.127.127.127.127.127.64.0/64.127.127.127.127.127.127.127.127.127.127.127.127.127.127.64"
    );
    // An odd triangle in flat red: where the samples sit, not just how many there are.
    let ops = draw_verts(FS_RED, &[-0.83, -0.71, 0.77, -0.33, -0.12, 0.91], TRIANGLES);
    assert_eq!(
        red_rows(&ops, 9, 7),
        "0.0.0.0.0.0.0.0.0/0.191.127.127.64.0.0.0.0/0.64.255.255.255.255.255.127.0/0.0.191.255.255.255.191.0.0/\
         0.0.64.255.255.255.0.0.0/0.0.0.191.255.64.0.0.0/0.0.0.64.64.0.0.0.0"
    );
}
