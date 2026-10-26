//! The Web Audio family — twenty-eight tables, and the seven constructor shapes among them.
//!
//! Written.

use deno_core::v8;

use super::rows::{build_newable, build_newable_with_data};
use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// The interfaces whose constructor is `new XNode(context, options)` — ONE callback keyed on the
/// interface NAME, which is why they go through [`build_newable_with_data`]. The list is the
/// `Model::Value` half of `webaudio/attrs.rs`'s tables; `ChannelMergerNode` and
/// `ChannelSplitterNode` are in `webaudio.rs`'s `NODE_CTORS` too but have no member of their own, so
/// they stay generated registry rows and take the same callback from `install::ctors::row_ctor`.
const NODE_TABLES: &[&str] = &[
    "OscillatorNode",
    "GainNode",
    "DynamicsCompressorNode",
    "AnalyserNode",
    "AudioBufferSourceNode",
    "ConstantSourceNode",
    "BiquadFilterNode",
    "StereoPannerNode",
    "WaveShaperNode",
    "DelayNode",
    "ConvolverNode",
    "PannerNode",
    "IIRFilterNode",
    "MediaElementAudioSourceNode",
    "MediaStreamAudioSourceNode",
    "MediaStreamAudioDestinationNode",
];

/// The whole Web Audio surface, from its tables.
///
/// WINDOW ONLY, and that is the capture's word: `worker.surface.json` lists not one interface of
/// this family, so there is no `install_worker_audio` and the name-based inventory takes nothing out
/// of the worker realm that was ever in it.
///
/// WHERE THIS RUNS, and why the sample rate is an argument. `wire_canvas_oracle` — the slot
/// `webaudio::attrs::install` ran from, at the end of `js/generated_shells.js`. `new AudioContext()`
/// has to know the device's sample rate, and re-parsing the device oracle inside the callback would
/// mean re-parsing it per `new`; the op has already parsed it, so it is bound as the constructor's
/// `data` here, exactly as the swap this file used to call bound it before (a later change retired that
/// swap; the sample rate is still an argument, for the reason above).
///
/// THE ORDER IS THE INHERITANCE ORDER. `installer::create_interface` and [`build_newable`] splice
/// onto a parent by reading `globalThis[parent].prototype`, so a parent has to be installed first:
/// `BaseAudioContext` before the two contexts, `AudioNode` before every node, and
/// `AudioScheduledSourceNode` before the three source nodes. `EventTarget`, `Event` and `Worklet`
/// are not this lane's and already exist.
pub fn install_window_audio<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    sample_rate: f32,
) {
    use crate::core::html::webaudio::attrs::{
        ANALYSERNODE, AUDIOBUFFER, AUDIOBUFFERSOURCENODE, AUDIOCONTEXT, AUDIODESTINATIONNODE,
        AUDIOLISTENER, AUDIONODE, AUDIOPARAM, AUDIOPROCESSINGEVENT, AUDIOSCHEDULEDSOURCENODE,
        BASEAUDIOCONTEXT, BIQUADFILTERNODE, CONSTANTSOURCENODE, CONVOLVERNODE, DELAYNODE,
        DYNAMICSCOMPRESSORNODE, GAINNODE, IIRFILTERNODE, MEDIAELEMENTAUDIOSOURCENODE,
        MEDIASTREAMAUDIODESTINATIONNODE, MEDIASTREAMAUDIOSOURCENODE, OFFLINEAUDIOCOMPLETIONEVENT,
        OFFLINEAUDIOCONTEXT, OSCILLATORNODE, PANNERNODE, SCRIPTPROCESSORNODE, STEREOPANNERNODE,
        WAVESHAPERNODE,
    };
    use crate::core::html::webaudio::{
        cb_new_audio_buffer, cb_new_audio_context, cb_new_audio_event, cb_new_audio_node,
        cb_new_offline_context,
    };

    let mut ledger = installer::take_ledger(scope);

    // 1. The three abstract bases and the two leaf interfaces with no constructor. A `Model::Interface`
    //    table's interface object is a real constructor that throws "Illegal constructor", which is
    //    what the capture records for all five (`"newable": false`, ctor `length` 0) and what the
    //    `cb_illegal_ctor` pass this replaces produced.
    for (def, parent) in [
        (&BASEAUDIOCONTEXT, Some("EventTarget")),
        (&AUDIONODE, Some("EventTarget")),
        (&AUDIOSCHEDULEDSOURCENODE, Some("AudioNode")),
        (&AUDIODESTINATIONNODE, Some("AudioNode")),
        (&AUDIOLISTENER, None),
        (&AUDIOPARAM, None),
        // `ScriptProcessorNode` has NO WebIDL constructor -- see the ruling in the commit that
        // wrote this. Its instances come from `BaseAudioContext.createScriptProcessor`.
        (&SCRIPTPROCESSORNODE, Some("AudioNode")),
    ] {
        create(scope, global, def, parent, &mut ledger);
    }

    // 2. The two contexts. `new AudioContext()` allocates the context's op list and registers its
    //    destination, and needs the persona's sample rate; `new OfflineAudioContext(...)` takes its
    //    own from the page.
    let rate: v8::Local<v8::Value> = v8::Number::new(scope, sample_rate as f64).into();
    build_newable_with_data(scope, global, &AUDIOCONTEXT, cb_new_audio_context, rate, Some("BaseAudioContext"), None, &mut ledger);
    let u: v8::Local<v8::Value> = v8::undefined(scope).into();
    build_newable_with_data(scope, global, &OFFLINEAUDIOCONTEXT, cb_new_offline_context, u, Some("BaseAudioContext"), None, &mut ledger);

    // 3. The sixteen node classes with a real `new XNode(context, options)`. One callback, keyed on
    //    the interface name, so the name is the constructor's `data` -- as `"<name>|<requiredArgs>"`,
    //    the interface-object data shape (`binding::protos::ctor_name` / `ctor_required`).
    //
    //    THE COUNT COMES FROM THE ROW. `cb_new_audio_node` used to spell "1 argument
    //    required" for all sixteen, and three of them require TWO -- `IIRFilterNode`,
    //    `MediaElementAudioSourceNode` and `MediaStreamAudioSourceNode`, whose second argument is a
    //    dictionary with required members. Their tables have said `len 2` all along (the capture
    //    recorded it off the browser) and the body disagreed with them; Chrome 150 quotes the table's
    //    number, measured over loopback 2026-09-12.
    for def in [
        &OSCILLATORNODE, &GAINNODE, &DYNAMICSCOMPRESSORNODE, &ANALYSERNODE, &AUDIOBUFFERSOURCENODE,
        &CONSTANTSOURCENODE, &BIQUADFILTERNODE, &STEREOPANNERNODE, &WAVESHAPERNODE, &DELAYNODE,
        &CONVOLVERNODE, &PANNERNODE, &IIRFILTERNODE, &MEDIAELEMENTAUDIOSOURCENODE,
        &MEDIASTREAMAUDIOSOURCENODE, &MEDIASTREAMAUDIODESTINATIONNODE,
    ] {
        debug_assert!(NODE_TABLES.contains(&def.name), "{} is not in NODE_TABLES", def.name);
        let l = def.ctor.as_ref().map(|c| c.length as i32).unwrap_or(1);
        let data: v8::Local<v8::Value> = crate::binding::protos::str_val(scope, &format!("{}|{l}", def.name));
        build_newable_with_data(scope, global, def, cb_new_audio_node, data, def.parent, None, &mut ledger);
    }

    // 4. `AudioBuffer` (no parent: `length` and `sampleRate` are REQUIRED dictionary members, so the
    //    empty form has to be a TypeError rather than an empty buffer) and the two audio EVENTS,
    //    which inherit `Event` and are built by one callback keyed on the interface name.
    build_newable(scope, global, &AUDIOBUFFER, cb_new_audio_buffer, None, None, &mut ledger);
    for def in [&AUDIOPROCESSINGEVENT, &OFFLINEAUDIOCOMPLETIONEVENT] {
        let data = crate::binding::protos::str_val(scope, def.name);
        build_newable_with_data(scope, global, def, cb_new_audio_event, data, Some("Event"), None, &mut ledger);
    }

    installer::put_ledger(scope, ledger);

    // 5. THE SIXTH STEP IS GONE, AND SO IS THE ORDERING HAZARD IT CARRIED. It called
    //    `webaudio::install_audio_shell_ctors`, which gave the family's six remaining generated rows
    //    a constructor by MINTING a new function per interface and assigning it over the global --
    //    and a minted function's `[[Prototype]]` is `Function.prototype`, so it had to run BEFORE the
    //    rechain below or it would undo the interface-OBJECT half of the chain the rechain had just
    //    repaired (measured: `tests/webaudio_live.rs::the_registry_children_of_audio_node_are_
    //    rechained` failed `true|false` with the two calls the other way round). The six are
    //    constructable from their rows now (`install::ctors::row_ctor`, built by the ONE create at
    //    registry-install time), so there is no second function and no order to get wrong.
    //
    // 6. Symmetry with L4/L14/L18: repair any registry child whose parent is a name this lane took
    //    out of the registry -- `AudioWorkletNode`, `ChannelMergerNode` and `ChannelSplitterNode`
    //    all inherit `AudioNode`, and the registry's own chain pass ran before these tables existed.
    //    Derived from the registry, never a list, and it touches only a child whose prototype is
    //    still the empty root.
    super::rows::rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
}

fn create<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    def: &'static InterfaceDef,
    parent: Option<&str>,
    ledger: &mut installer::InstallLedger,
) {
    let parent = parent.and_then(|p| super::rows::parent_of(scope, p));
    let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
    for g in &built.gaps {
        eprintln!("[korvus] audio install gap: {}.{}", g.interface, g.member);
    }
}
