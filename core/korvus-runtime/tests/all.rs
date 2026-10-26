//! Every integration test of this crate, in ONE binary.
//!
//! WHY. Cargo compiles each `tests/*.rs` into its own executable, and each one statically links the
//! whole dependency graph -- deno_core and V8 included. Measured 2026-09-11 on this crate: 118 separate
//! binaries, 23.5 GB of linker output (204 MB each) for ONE `cargo test`; the same tests as a single
//! binary are 215 MB. That is a 109x reduction in bytes written per full build, and the autonomous loop
//! runs full builds all day -- it was writing ~3.6 TB/day to a consumer NVMe, which no such drive
//! survives.
//!
//! THE FILES DO NOT MOVE. `#[path]` keeps every test where it was, which matters because `include_str!`
//! resolves relative to the file containing it: 86 such call sites across this repo keep working with
//! no edit. Do not "tidy" these into a `tests/suites/` directory -- that would break every one of them.
//!
//! ADDING A TEST. Drop the file in `tests/` and add a line below. `every_test_file_is_in_this_suite`
//! fails if you forget, because a test file that is never compiled is worse than no test at all: it
//! looks like coverage and is not.
//!
//! WHAT IS DELIBERATELY NOT HERE, and why (one process = shared state):
//!   * `no_page_plugin.rs` -- it does `env::set_var("KORVUS_NO_PAGE", "1")`, and `lib.rs` reads that
//!     variable LIVE on every `Engine` construction (not cached). In one process, any other test that
//!     builds an Engine inside that window silently loses the page plugin and sees zero geometry. Its
//!     own comment already says it depends on file-level process isolation. It stays a separate binary,
//!     declared in Cargo.toml. The real fix is an `Engine` option instead of an env switch; until then
//!     this is the honest arrangement, and it costs one binary out of 118.
//!   * `install_ledger.rs` -- it READS the install ledger, which is process-global because
//!     a worker realm is assembled on another thread. In one process every other test's `Engine`
//!     writes into that same ledger, in configurations this gate never asked for, and its answer then
//!     depends on which engine ran last: measured at 41 second writers alone and 36 inside this
//!     binary, three rows appearing and eight disappearing. Its own process is the only way its
//!     number means anything. Declared in Cargo.toml.
#[path = "abort_live.rs"] mod abort_live;
#[path = "abort_signal.rs"] mod abort_signal;
#[path = "adjacent.rs"] mod adjacent;
#[path = "anim_live.rs"] mod anim_live;
#[path = "animation.rs"] mod animation;
#[path = "arity_guards.rs"] mod arity_guards;
#[path = "audio_parity.rs"] mod audio_parity;
#[path = "automation_tells.rs"] mod automation_tells;
#[path = "base_url_live.rs"] mod base_url_live;
#[path = "behavior.rs"] mod behavior;
#[path = "bodies_live.rs"] mod bodies_live;
#[path = "blank_frame_load_live.rs"] mod blank_frame_load_live;
#[path = "blank_frame_document_live.rs"] mod blank_frame_document_live;
#[path = "computed_style_live.rs"] mod computed_style_live;
#[path = "idle_callback_live.rs"] mod idle_callback_live;
#[path = "names_and_error_sites_live.rs"] mod names_and_error_sites_live;
#[path = "blockify_live.rs"] mod blockify_live;
#[path = "bridge_slots.rs"] mod bridge_slots;
#[path = "canvas_2d_live.rs"] mod canvas_2d_live;
#[path = "canvas_draw_image_live.rs"] mod canvas_draw_image_live;
#[path = "chardata.rs"] mod chardata;
#[path = "chrome_object_live.rs"] mod chrome_object_live;
#[path = "check_visibility_media_live.rs"] mod check_visibility_media_live;
#[path = "container_animations_live.rs"] mod container_animations_live;
#[path = "css_time_live.rs"] mod css_time_live;
#[path = "css_animation_events_live.rs"] mod css_animation_events_live;
#[path = "font_probe_live.rs"] mod font_probe_live;
#[path = "shadow_animations_live.rs"] mod shadow_animations_live;
#[path = "slotted_transition_live.rs"] mod slotted_transition_live;
#[path = "classlist.rs"] mod classlist;
#[path = "console_calls_live.rs"] mod console_calls_live;
#[path = "client_hints.rs"] mod client_hints;
#[path = "clone_void.rs"] mod clone_void;
#[path = "collections.rs"] mod collections;
#[path = "collections_live.rs"] mod collections_live;
#[path = "combinators.rs"] mod combinators;
#[path = "compression_live.rs"] mod compression_live;
#[path = "computed_style.rs"] mod computed_style;
#[path = "conformance.rs"] mod conformance;
#[path = "contains.rs"] mod contains;
#[path = "cookies.rs"] mod cookies;
#[path = "cookies_live.rs"] mod cookies_live;
#[path = "corpus_worklist_live.rs"] mod corpus_worklist_live;
#[path = "crypto.rs"] mod crypto;
#[path = "current_script_checkpoint_live.rs"] mod current_script_checkpoint_live;
#[path = "css_attrs.rs"] mod css_attrs;
#[path = "css_selectors.rs"] mod css_selectors;
#[path = "cssom.rs"] mod cssom;
#[path = "css_anim_live.rs"] mod css_anim_live;
#[path = "cssom_live.rs"] mod cssom_live;
#[path = "dataset_headers.rs"] mod dataset_headers;
#[path = "datatransfer_live.rs"] mod datatransfer_live;
#[path = "discover.rs"] mod discover;
#[path = "doc_body.rs"] mod doc_body;
#[path = "document_all_live.rs"] mod document_all_live;
#[path = "document_readiness.rs"] mod document_readiness;
#[path = "document_write_live.rs"] mod document_write_live;
#[path = "dom_bindings.rs"] mod dom_bindings;
#[path = "dom_create.rs"] mod dom_create;
#[path = "dom_events.rs"] mod dom_events;
#[path = "dom_exception_live.rs"] mod dom_exception_live;
#[path = "dom_insert.rs"] mod dom_insert;
#[path = "dom_manip.rs"] mod dom_manip;
#[path = "dom_mutate.rs"] mod dom_mutate;
#[path = "dom_nav.rs"] mod dom_nav;
#[path = "dom_matrix_live.rs"] mod dom_matrix_live;
#[path = "dom_point_live.rs"] mod dom_point_live;
#[path = "dom_point_worker_live.rs"] mod dom_point_worker_live;
#[path = "dom_quad_live.rs"] mod dom_quad_live;
#[path = "dom_rect_live.rs"] mod dom_rect_live;
#[path = "dom_rect_worker_live.rs"] mod dom_rect_worker_live;
#[path = "dom_remove.rs"] mod dom_remove;
#[path = "dom_spine_live.rs"] mod dom_spine_live;
#[path = "dom_traverse.rs"] mod dom_traverse;
#[path = "domparser.rs"] mod domparser;
#[path = "error_reporting_live.rs"] mod error_reporting_live;
#[path = "event_family_live.rs"] mod event_family_live;
#[path = "event_model_live.rs"] mod event_model_live;
#[path = "event_path_live.rs"] mod event_path_live;
#[path = "events.rs"] mod events;
#[path = "crash_report_live.rs"] mod crash_report_live;
#[path = "feature_policy_live.rs"] mod feature_policy_live;
#[path = "fetch_types_live.rs"] mod fetch_types_live;
#[path = "fileapi_live.rs"] mod fileapi_live;
#[path = "flush_ready.rs"] mod flush_ready;
#[path = "font_face_live.rs"] mod font_face_live;
#[path = "font_match_live.rs"] mod font_match_live;
#[path = "document_fonts_live.rs"] mod document_fonts_live;
#[path = "fetch_values_live.rs"] mod fetch_values_live;
#[path = "formapis.rs"] mod formapis;
#[path = "formdata.rs"] mod formdata;
#[path = "forms_live.rs"] mod forms_live;
#[path = "frame_csp_live.rs"] mod frame_csp_live;
#[path = "cross_origin_isolation_live.rs"] mod cross_origin_isolation_live;
#[path = "wire_spelling_live.rs"] mod wire_spelling_live;
#[path = "frame_time_origin_live.rs"] mod frame_time_origin_live;
#[path = "request_start_live.rs"] mod request_start_live;
#[path = "module_resource_timing_live.rs"] mod module_resource_timing_live;
#[path = "worker_start_live.rs"] mod worker_start_live;
#[path = "frame_globals_live.rs"] mod frame_globals_live;
#[path = "fragment.rs"] mod fragment;
#[path = "global_order.rs"] mod global_order;
#[path = "globals.rs"] mod globals;
#[path = "has_selector.rs"] mod has_selector;
#[path = "header_guard_live.rs"] mod header_guard_live;
#[path = "highlight_live.rs"] mod highlight_live;
#[path = "history.rs"] mod history;
#[path = "hit_test.rs"] mod hit_test;
#[path = "hover_live.rs"] mod hover_live;
#[path = "html_image_live.rs"] mod html_image_live;
#[path = "iface_object_live.rs"] mod iface_object_live;
#[path = "idb_live.rs"] mod idb_live;
#[path = "image_data_live.rs"] mod image_data_live;
#[path = "implementation.rs"] mod implementation;
#[path = "import_defer.rs"] mod import_defer;
#[path = "import_map_live.rs"] mod import_map_live;
#[path = "inline_live.rs"] mod inline_live;
#[path = "inner_text_live.rs"] mod inner_text_live;
#[path = "inner_text_rendered_live.rs"] mod inner_text_rendered_live;
#[path = "inline_module_url.rs"] mod inline_module_url;
#[path = "inline_script.rs"] mod inline_script;
#[path = "inserted_module_live.rs"] mod inserted_module_live;
#[path = "js.rs"] mod js;
#[path = "l10_live.rs"] mod l10_live;
#[path = "linked_stylesheet_live.rs"] mod linked_stylesheet_live;
#[path = "stylesheet_link_states_live.rs"] mod stylesheet_link_states_live;
#[path = "stylesheet_sheet_lifecycle_live.rs"] mod stylesheet_sheet_lifecycle_live;
#[path = "stylesheet_sheet_identity_live.rs"] mod stylesheet_sheet_identity_live;
#[path = "media_devices_live.rs"] mod media_devices_live;
#[path = "named_index_live.rs"] mod named_index_live;
#[path = "layout_invalidation_live.rs"] mod layout_invalidation_live;
#[path = "navigation_api_live.rs"] mod navigation_api_live;
#[path = "frame_async_fetch_live.rs"] mod frame_async_fetch_live;
#[path = "stylesheet_loads_live.rs"] mod stylesheet_loads_live;
#[path = "font_request_per_document_live.rs"] mod font_request_per_document_live;
#[path = "written_preload_scan_live.rs"] mod written_preload_scan_live;
#[path = "image_document_request_live.rs"] mod image_document_request_live;
#[path = "style_var_urls_and_font_ranges_live.rs"] mod style_var_urls_and_font_ranges_live;
#[path = "css_url_loads_live.rs"] mod css_url_loads_live;
#[path = "load_delay.rs"] mod load_delay;
#[path = "load_delay_chrome_live.rs"] mod load_delay_chrome_live;
#[path = "l11_live.rs"] mod l11_live;
#[path = "l11b_live.rs"] mod l11b_live;
#[path = "l11c_live.rs"] mod l11c_live;
#[path = "l12_live.rs"] mod l12_live;
#[path = "l14_missing_apis_live.rs"] mod l14_missing_apis_live;
#[path = "layers_live.rs"] mod layers_live;
#[path = "layout_metrics.rs"] mod layout_metrics;
#[path = "location.rs"] mod location;
#[path = "looking_elements_live.rs"] mod looking_elements_live;
#[path = "media_query_list_live.rs"] mod media_query_list_live;
#[path = "media_live.rs"] mod media_live;
#[path = "media_objects_live.rs"] mod media_objects_live;
#[path = "media_source_live.rs"] mod media_source_live;
#[path = "message_port_transfer_live.rs"] mod message_port_transfer_live;
#[path = "module_drain_live.rs"] mod module_drain_live;
#[path = "mouse_events.rs"] mod mouse_events;
#[path = "mouse_input_live.rs"] mod mouse_input_live;
#[path = "native_binding.rs"] mod native_binding;
#[path = "navigator_live.rs"] mod navigator_live;
#[path = "navigator_shape.rs"] mod navigator_shape;
#[path = "nested_contexts.rs"] mod nested_contexts;
#[path = "net.rs"] mod net;
#[path = "no_mask_live.rs"] mod no_mask_live;
#[path = "node_compare.rs"] mod node_compare;
#[path = "node_nav.rs"] mod node_nav;
#[path = "nolies.rs"] mod nolies;
#[path = "nomodule_and_script_task_source_live.rs"] mod nomodule_and_script_task_source_live;
#[path = "nth.rs"] mod nth;
#[path = "observers.rs"] mod observers;
#[path = "observer_delivery_live.rs"] mod observer_delivery_live;
#[path = "obstacle_course.rs"] mod obstacle_course;
#[path = "of_type.rs"] mod of_type;
#[path = "one_v8_init.rs"] mod one_v8_init;
#[path = "options_length.rs"] mod options_length;
#[path = "oracle_data.rs"] mod oracle_data;
#[path = "oracle_local.rs"] mod oracle_local;
#[path = "outer_value.rs"] mod outer_value;
#[path = "outerhtml_set.rs"] mod outerhtml_set;
#[path = "paint_holding_live.rs"] mod paint_holding_live;
#[path = "parser_subresources.rs"] mod parser_subresources;
#[path = "fetch_semantics_live.rs"] mod fetch_semantics_live;
#[path = "perf_abort.rs"] mod perf_abort;
#[path = "navigation_timing_live.rs"] mod navigation_timing_live;
#[path = "nested_workers_live.rs"] mod nested_workers_live;
#[path = "performance_entries_live.rs"] mod performance_entries_live;
#[path = "performance_live.rs"] mod performance_live;
#[path = "performance_memory_live.rs"] mod performance_memory_live;
#[path = "performance_timeline.rs"] mod performance_timeline;
#[path = "performance_timing_live.rs"] mod performance_timing_live;
#[path = "persona_gate.rs"] mod persona_gate;
#[path = "persona_layer.rs"] mod persona_layer;
#[path = "windows_persona.rs"] mod windows_persona;
#[path = "postshells_live.rs"] mod postshells_live;
#[path = "pseudo.rs"] mod pseudo;
#[path = "query.rs"] mod query;
#[path = "range_live.rs"] mod range_live;
#[path = "readable_stream_async.rs"] mod readable_stream_async;
#[path = "receiver_reject.rs"] mod receiver_reject;
#[path = "receiver_sweep.rs"] mod receiver_sweep;
#[path = "reflected.rs"] mod reflected;
#[path = "rejection_live.rs"] mod rejection_live;
#[path = "streaming_parse_live.rs"] mod streaming_parse_live;
#[path = "frame_mute_load_live.rs"] mod frame_mute_load_live;
#[path = "timer_realm_live.rs"] mod timer_realm_live;
#[path = "port_realm_live.rs"] mod port_realm_live;
#[path = "opaque_origin_live.rs"] mod opaque_origin_live;
#[path = "frame_load_waits_live.rs"] mod frame_load_waits_live;
#[path = "dangling_markup_live.rs"] mod dangling_markup_live;
#[path = "ctor_chain_live.rs"] mod ctor_chain_live;
#[path = "resource_timing_cors_live.rs"] mod resource_timing_cors_live;
#[path = "frame_resource_timing_live.rs"] mod frame_resource_timing_live;
#[path = "refused_load_timing_live.rs"] mod refused_load_timing_live;
#[path = "origin_dirty_sheet_timing_live.rs"] mod origin_dirty_sheet_timing_live;
#[path = "sandboxed_frames_live.rs"] mod sandboxed_frames_live;
#[path = "render.rs"] mod render;
#[path = "render_calibration.rs"] mod render_calibration;
#[path = "replace_children.rs"] mod replace_children;
#[path = "root_node_live.rs"] mod root_node_live;
#[path = "rtc_graph_live.rs"] mod rtc_graph_live;
#[path = "rtc_live.rs"] mod rtc_live;
#[path = "script_response_live.rs"] mod script_response_live;
#[path = "script_stream_live.rs"] mod script_stream_live;
#[path = "listener_microtasks_live.rs"] mod listener_microtasks_live;
#[path = "listener_snapshot_live.rs"] mod listener_snapshot_live;
#[path = "is_trusted_shape_live.rs"] mod is_trusted_shape_live;
#[path = "select_value.rs"] mod select_value;
#[path = "scroll_live.rs"] mod scroll_live;
#[path = "scrollbars_live.rs"] mod scrollbars_live;
#[path = "screen_orientation.rs"] mod screen_orientation;
#[path = "generated_content_live.rs"] mod generated_content_live;
#[path = "geometry_scroll_frames_live.rs"] mod geometry_scroll_frames_live;
#[path = "animation_timeline_sample_live.rs"] mod animation_timeline_sample_live;
#[path = "stylesheet_state_live.rs"] mod stylesheet_state_live;
#[path = "service_worker_live.rs"] mod service_worker_live;
#[path = "selection.rs"] mod selection;
#[path = "shadow_dom.rs"] mod shadow_dom;
#[path = "shadow_slots.rs"] mod shadow_slots;
#[path = "shared_globals_live.rs"] mod shared_globals_live;
#[path = "shell_proto_seal_live.rs"] mod shell_proto_seal_live;
#[path = "shared_worker_live.rs"] mod shared_worker_live;
#[path = "singletons.rs"] mod singletons;
#[path = "small_elements_live.rs"] mod small_elements_live;
#[path = "storage.rs"] mod storage;
#[path = "stream_closed_live.rs"] mod stream_closed_live;
#[path = "style.rs"] mod style;
#[path = "subresource.rs"] mod subresource;
#[path = "svg_ns.rs"] mod svg_ns;
#[path = "svg_string_list_live.rs"] mod svg_string_list_live;
#[path = "svg_text_live.rs"] mod svg_text_live;
#[path = "editing_commands_live.rs"] mod editing_commands_live;
#[path = "sizing_live.rs"] mod sizing_live;
#[path = "frame_documents_live.rs"] mod frame_documents_live;
#[path = "platform_answers_live.rs"] mod platform_answers_live;
#[path = "xhr_streaming_live.rs"] mod xhr_streaming_live;
#[path = "connection_partition_live.rs"] mod connection_partition_live;
#[path = "connect_timing_live.rs"] mod connect_timing_live;
#[path = "foreign_receivers_live.rs"] mod foreign_receivers_live;
#[path = "http_cache_live.rs"] mod http_cache_live;
#[path = "chrome_frame_windows_live.rs"] mod chrome_frame_windows_live;
#[path = "device_sensors_live.rs"] mod device_sensors_live;
#[path = "chrome_opfs_timing_live.rs"] mod chrome_opfs_timing_live;
#[path = "chrome_heap_live.rs"] mod chrome_heap_live;
#[path = "chrome_canvas_and_errors_live.rs"] mod chrome_canvas_and_errors_live;
#[path = "chrome_iteration_live.rs"] mod chrome_iteration_live;
#[path = "frame_tasks_live.rs"] mod frame_tasks_live;
#[path = "svg_values.rs"] mod svg_values;
#[path = "table_live.rs"] mod table_live;
#[path = "template_clipboard_aria_live.rs"] mod template_clipboard_aria_live;
#[path = "templates_blueprint_live.rs"] mod templates_blueprint_live;
#[path = "text_metrics.rs"] mod text_metrics;
#[path = "textnode.rs"] mod textnode;
#[path = "timers.rs"] mod timers;
#[path = "track_element_live.rs"] mod track_element_live;
#[path = "treewalker.rs"] mod treewalker;
#[path = "typed_om_view_transition_live.rs"] mod typed_om_view_transition_live;
#[path = "typing_live.rs"] mod typing_live;
#[path = "key_events_live.rs"] mod key_events_live;
#[path = "scheduler_live.rs"] mod scheduler_live;
#[path = "trusted_types_live.rs"] mod trusted_types_live;
#[path = "trusted_types_enforced_live.rs"] mod trusted_types_enforced_live;
#[path = "structured_clone_live.rs"] mod structured_clone_live;
#[path = "url_live.rs"] mod url_live;
#[path = "url_mutable.rs"] mod url_mutable;
#[path = "values_parity.rs"] mod values_parity;
#[path = "visual_viewport_live.rs"] mod visual_viewport_live;
#[path = "web_apis.rs"] mod web_apis;
#[path = "webaudio_live.rs"] mod webaudio_live;
#[path = "webgl_live.rs"] mod webgl_live;
#[path = "webgpu_live.rs"] mod webgpu_live;
#[path = "websocket_live.rs"] mod websocket_live;
#[path = "webgl_parity.rs"] mod webgl_parity;
#[path = "window_dims.rs"] mod window_dims;
#[path = "window_document_live.rs"] mod window_document_live;
#[path = "window_extras_live.rs"] mod window_extras_live;
#[path = "window_exposure_live.rs"] mod window_exposure_live;
#[path = "window_globals_live.rs"] mod window_globals_live;
#[path = "window_surface_live.rs"] mod window_surface_live;
#[path = "codec_grid_live.rs"] mod codec_grid_live;
#[path = "cssom_text_live.rs"] mod cssom_text_live;
#[path = "webgl_state_live.rs"] mod webgl_state_live;
#[path = "text_metrics_live.rs"] mod text_metrics_live;
#[path = "canvas_text_pixels_live.rs"] mod canvas_text_pixels_live;
#[path = "audio_render_live.rs"] mod audio_render_live;
#[path = "builtin_fast_paths_live.rs"] mod builtin_fast_paths_live;
#[path = "layout_root_live.rs"] mod layout_root_live;
#[path = "form_controls_live.rs"] mod form_controls_live;
#[path = "transforms_live.rs"] mod transforms_live;
#[path = "inline_layout_live.rs"] mod inline_layout_live;
#[path = "tables_live.rs"] mod tables_live;
#[path = "keyboard_live.rs"] mod keyboard_live;
#[path = "ctor_dicts_live.rs"] mod ctor_dicts_live;
#[path = "dict_conversion_live.rs"] mod dict_conversion_live;
#[path = "frozen_arrays_live.rs"] mod frozen_arrays_live;
#[path = "idl_values_live.rs"] mod idl_values_live;
#[path = "css_answers_live.rs"] mod css_answers_live;
#[path = "content_type_live.rs"] mod content_type_live;
#[path = "content_coding_live.rs"] mod content_coding_live;
#[path = "h2_priority_live.rs"] mod h2_priority_live;
#[path = "connection_estimate_live.rs"] mod connection_estimate_live;
#[path = "frame_charset_live.rs"] mod frame_charset_live;
#[path = "navigation_events_live.rs"] mod navigation_events_live;
#[path = "webgl2_state_live.rs"] mod webgl2_state_live;
#[path = "svg_values_live.rs"] mod svg_values_live;
#[path = "challenge_batch_live.rs"] mod challenge_batch_live;
#[path = "challenge_probes_side_by_side_live.rs"] mod challenge_probes_side_by_side_live;
#[path = "webgl_extension_shapes_live.rs"] mod webgl_extension_shapes_live;
// what a page reads of its WebGL device: a helper the WebGL tests share, no tests of its own
#[path = "webgl_device.rs"] mod webgl_device;
#[path = "webgl_gpu_live.rs"] mod webgl_gpu_live;
#[path = "engine_reads_live.rs"] mod engine_reads_live;
#[path = "xml_serializer_live.rs"] mod xml_serializer_live;
#[path = "dir_attribute_live.rs"] mod dir_attribute_live;
#[path = "broken_img_live.rs"] mod broken_img_live;
#[path = "stack_format_live.rs"] mod stack_format_live;
#[path = "media_mime_live.rs"] mod media_mime_live;
#[path = "media_answers_live.rs"] mod media_answers_live;
#[path = "canvas_encode_live.rs"] mod canvas_encode_live;
#[path = "webgl_multisample_live.rs"] mod webgl_multisample_live;
#[path = "microtask_checkpoints_live.rs"] mod microtask_checkpoints_live;
#[path = "csp_violations_live.rs"] mod csp_violations_live;
#[path = "dom_window_answers_live.rs"] mod dom_window_answers_live;
#[path = "presentational_hints_live.rs"] mod presentational_hints_live;
#[path = "frame_status_live.rs"] mod frame_status_live;
#[path = "put_forwards_live.rs"] mod put_forwards_live;
#[path = "style_sheets_live.rs"] mod style_sheets_live;
#[path = "timer_timing_live.rs"] mod timer_timing_live;
#[path = "task_order_live.rs"] mod task_order_live;
#[path = "desktop_session_live.rs"] mod desktop_session_live;
#[path = "persona_surface.rs"] mod persona_surface;
#[path = "client_hints_live.rs"] mod client_hints_live;
#[path = "request_shape_live.rs"] mod request_shape_live;
#[path = "server_timing_live.rs"] mod server_timing_live;
#[path = "webrtc_live.rs"] mod webrtc_live;
#[path = "worker.rs"] mod worker;
#[path = "worker_fidelity_live.rs"] mod worker_fidelity_live;
#[path = "worker_live.rs"] mod worker_live;
#[path = "worker_navigator_live.rs"] mod worker_navigator_live;
#[path = "worker_scope_live.rs"] mod worker_scope_live;
#[path = "worker_script_loading_live.rs"] mod worker_script_loading_live;
#[path = "worker_url_loading.rs"] mod worker_url_loading;
#[path = "worker_xhr.rs"] mod worker_xhr;
#[path = "ctor_messages_live.rs"] mod ctor_messages_live;
#[path = "node_tables_reinstall.rs"] mod node_tables_reinstall;
#[path = "wasm_streaming_live.rs"] mod wasm_streaming_live;
#[path = "webcrypto_live.rs"] mod webcrypto_live;
#[path = "which_subresources_load.rs"] mod which_subresources_load;
#[path = "wpt.rs"] mod wpt;
#[path = "xhr.rs"] mod xhr;
#[path = "xml_viewer_live.rs"] mod xml_viewer_live;

/// A test file that is not listed above is never compiled, and absence of a test looks exactly like a
/// passing test. This reads the directory and the suite's own source, so forgetting a line fails here.
#[test]
fn every_test_file_is_in_this_suite() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let me = std::fs::read_to_string(dir.join("all.rs")).expect("read tests/all.rs");
    // Declared in Cargo.toml as its own binary; the header says why.
    const SEPARATE: &[&str] = &["no_page_plugin", "install_ledger"];
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read tests/") {
        let name = entry.expect("dir entry").file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".rs") else { continue };
        if stem == "all" || SEPARATE.contains(&stem) {
            continue;
        }
        if !me.contains(&format!("#[path = \"{stem}.rs\"]")) {
            missing.push(stem.to_string());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "these test files are not in tests/all.rs, so they are NEVER COMPILED OR RUN: {missing:?}\n\
         add `#[path = \"<name>.rs\"] mod <name>;` for each, or list it in SEPARATE with a reason."
    );
}
#[path = "media_load_live.rs"] mod media_load_live;
#[path = "realm_globals_live.rs"] mod realm_globals_live;
#[path = "video_decode_live.rs"] mod video_decode_live;
