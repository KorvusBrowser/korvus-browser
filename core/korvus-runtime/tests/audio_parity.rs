//! R3.5 (render spine, audio slice): an `OfflineAudioContext` renders its GRAPH, `getChannelData`
//! returns those computed samples, and an `AnalyserNode` reports the spectrum of what is actually
//! connected to it.
//!
//! ## What this test used to assert, and why every changed expectation had to change
//!
//! It asserted the TAPE. Two of its four expected lines were recordings, and the rewrite is part of
//! Track 3.5 rather than a quiet edit of expected values:
//!
//! ```text
//! was  "nodes=number,triangle,-19.004104614257812"
//! now  "nodes=number,triangle,0"  before the render, and "reduction<0=true" after it
//!      -19.004104614257812 was `deviceOracle.audio.reduction`, one recorded constant that every
//!      DynamicsCompressorNode reported for every graph, every threshold and every input -- and it
//!      was reported at graph-construction time, before any audio existed. `reduction` is now
//!      computed by the envelope follower that produced the samples, which makes the ORDER
//!      observable and correct: a compressor that has not run yet is reducing nothing, so it reads
//!      0, and one that has just rendered a 0 dBFS triangle over a -50 dB threshold reads tens of
//!      dB down. Pinning the post-render number to a literal would pin this host's DSP output,
//!      which is not what the assertion is for.
//!
//! was  "buf=44100,true,true"
//! now  "buf=44100,true,true,true"
//!      The old third field asserted `getChannelData(0)[4500] === d[4500]` -- i.e. that two calls
//!      agree -- which a tape satisfies trivially and which is not evidence of anything. It stays
//!      (a browser hands back the same array, and now it literally is the same object), and a
//!      fourth field is added for the thing the tape could not do: the same graph at two
//!      frequencies must give two different fingerprint sums.
//! ```
//!
//! The native-ness assertions are UNCHANGED. They are stealth checks -- a JS shim on
//! `createOscillator` or `startRendering` is a one-expression tell -- and they were correct.
//!
//! Bit-identity with Brave is explicitly not asserted anywhere here. Brave farbles audio, the
//! persona claims a Mac and this host is Linux; the bar is that the samples are a function of the
//! graph. See `docs/RENDER-SPINE.md`.
use engine::Engine;

#[tokio::test]
async fn audio_offline_context_fingerprint() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        print("ctor=" + (typeof OfflineAudioContext) + "," + OfflineAudioContext.length);
        function graph(freq) {
            const ctx = new OfflineAudioContext(1, 44100, 44100);
            const osc = ctx.createOscillator();
            osc.type = "triangle"; osc.frequency.value = freq;
            const comp = ctx.createDynamicsCompressor();
            comp.threshold.value = -50; comp.ratio.value = 12;
            osc.connect(comp); comp.connect(ctx.destination); osc.start(0);
            return { ctx: ctx, osc: osc, comp: comp };
        }
        const g = graph(10000);
        // Read BEFORE the render: nothing has been processed, so the compressor is reducing
        // nothing. The tape reported -19.004104614257812 here, for a compressor that had not run.
        print("nodes=" + (typeof g.osc.frequency.value) + "," + g.osc.type + "," + g.comp.reduction);
        // getContext/graph methods must be NATIVE (a JS shim is a stealth tell).
        print("native=" + g.ctx.createOscillator.toString().includes("[native code]") + "," + g.ctx.startRendering.toString().includes("[native code]"));
        function sum(buf) {
            const d = buf.getChannelData(0);
            let s = 0; for (let i = 4500; i < 5000; i++) s += Math.abs(d[i]);
            return s;
        }
        g.ctx.startRendering().then(function (buf) {
            const d = buf.getChannelData(0);
            const other = graph(1000);
            return other.ctx.startRendering().then(function (buf2) {
                print("reduction<0=" + (g.comp.reduction < 0));
                print("buf=" + d.length + "," + (sum(buf) > 0) + "," + (buf.getChannelData(0)[4500] === d[4500]) + "," + (sum(buf) !== sum(buf2)));
            });
        });
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "ctor=function,1",
        "nodes=number,triangle,0",
        "native=true,true",
        "reduction<0=true",
        "buf=44100,true,true,true",
    ]);
}

/// The eight first-order attributes that all read `undefined` before this track. `sampleRate` alone
/// is read by every audio-fingerprinting script that exists.
#[tokio::test]
async fn audio_context_attributes_are_real() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        const ac = new AudioContext();
        print("state=" + ac.state);
        print("sampleRate=" + ac.sampleRate);
        // 512 frames at the device rate -- the relation, not a number written down. At the persona's
        // 48 kHz this is 0.010666666666666666, which is what Brave 151 reports on this machine.
        print("baseLatency=" + ac.baseLatency);
        // NOT `currentTime === 0`, WHICH WAS AN ASSERTION ABOUT THIS BOX'S SPEED. A live context's
        // clock is `created.elapsed() * sampleRate`, floored to whole 128-frame quanta
        // (`webaudio.rs::position`), and one quantum at the persona's 48 kHz is 2.667 ms — so the old
        // literal required this machine to get from `new AudioContext()` to here in under 2.7 ms, and
        // under a loaded gate run it does not. It went red exactly once that way
        // (a gate, 825/1) and green in three full-suite runs at the same commit.
        //
        // What is actually being asserted, and it is STRONGER than the literal it replaces: the clock
        // starts at the beginning and advances only in whole quanta. `q` is 0 on a fast run and 1 or 2
        // on a slow one, and a context whose clock drifted off the quantum grid — or ran away — fails.
        const q = ac.currentTime * ac.sampleRate / 128;
        print("currentTime=" + (Number.isInteger(q) && q >= 0 && q < 16));
        print("destination=" + Object.prototype.toString.call(ac.destination) + "," + ac.destination.channelCount);
        const oc = new OfflineAudioContext(2, 1024, 44100);
        // An OfflineAudioContext's rate is its CONSTRUCTOR's third argument, per spec, and never the
        // persona's -- which is why this reads 44100 while the live context above reads 48000.
        print("offline=" + oc.length + "," + oc.sampleRate + "," + oc.state);
        // A real browser's audio nodes have NO own properties; every attribute is a prototype
        // accessor, and `osc.frequency` is the same object every read.
        const osc = ac.createOscillator();
        print("shape=" + Object.getOwnPropertyNames(osc).length + "," + (osc.frequency === osc.frequency));
        print("params=" + osc.frequency.value + "," + osc.frequency.defaultValue + "," + osc.detune.minValue);
    "#.into()).unwrap();
    e.run().await.unwrap();
    // `suspended`, not `running`: with no user gesture the autoplay policy holds a live context, and
    // its clock stays at 0 (Brave 151 and Chrome 150 headed, 2026-10-01).
    assert_eq!(e.take_output(), vec![
        "state=suspended",
        "sampleRate=48000",
        "baseLatency=0.010666666666666666",
        "currentTime=true",
        "destination=[object AudioDestinationNode],2",
        "offline=1024,44100,suspended",
        "shape=0,true",
        "params=440,440,-153600",
    ]);
}

/// The three one-expression detections the tape carried, each verified live against Brave 151 on
/// this machine before the rewrite. All three are properties of the signal, so all three are gone
/// the moment the signal is computed rather than tiled.
#[tokio::test]
async fn the_three_tape_detections_are_gone() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        // 1. A fresh AnalyserNode with NOTHING connected. Brave: -Infinity, because no audio has
        //    been processed. The tape: -170.02041625976562, a plausible noise floor that was not
        //    there. Needs no audio, no timing and no graph -- one expression.
        const ac = new AudioContext();
        const idle = ac.createAnalyser();
        const f0 = new Float32Array(idle.frequencyBinCount);
        idle.getFloatFrequencyData(f0);
        print("idle=" + f0[0] + "," + f0[512]);

        // 2. Exact periodicity in the spectrum. frequencyBinCount is 1024 and the tape was 256, so
        //    f[0] === f[256] === f[512] === f[768] held exactly. Real spectral data does not.
        const oc = new OfflineAudioContext(1, 44100, 44100);
        const osc = oc.createOscillator();
        osc.type = "triangle"; osc.frequency.value = 10000;
        const an = oc.createAnalyser();
        an.smoothingTimeConstant = 0;
        osc.connect(an); an.connect(oc.destination); osc.start(0);
        oc.startRendering().then(function (buf) {
            const f = new Float32Array(an.frequencyBinCount);
            an.getFloatFrequencyData(f);
            print("period=" + (f[0] === f[256] && f[256] === f[512] && f[512] === f[768]));

            // 3. The same tiling on getChannelData: d[i] === d[i+256] across all 44,100 samples,
            //    which puts a 44,100-sample buffer's whole energy at multiples of 172.27 Hz. A
            //    triangle wave through a compressor does not look remotely like that.
            const d = buf.getChannelData(0);
            let same = 0;
            for (let i = 0; i < d.length - 256; i++) if (d[i] === d[i + 256]) same++;
            print("tiled=" + (same > d.length / 2) + "," + (same < 1000));
        });
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "idle=-Infinity,-Infinity",
        "period=false",
        "tiled=false,true",
    ]);
}

/// `getChannelData` and `getFloatFrequencyData` must describe ONE graph. The 2D path had exactly
/// this split -- `getImageData` real while `toDataURL` replayed a recording -- and it was a
/// single-expression tell. Here both come out of one `Engine` run over one op list, so a change to
/// the graph has to move both or neither.
#[tokio::test]
async fn the_two_readbacks_agree_about_the_graph() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        function measure(freq) {
            const oc = new OfflineAudioContext(1, 16384, 44100);
            const osc = oc.createOscillator();
            osc.type = "sine"; osc.frequency.value = freq;
            const an = oc.createAnalyser();
            an.fftSize = 2048; an.smoothingTimeConstant = 0;
            osc.connect(an); an.connect(oc.destination); osc.start(0);
            return oc.startRendering().then(function (buf) {
                const f = new Float32Array(an.frequencyBinCount);
                an.getFloatFrequencyData(f);
                let peak = 0;
                for (let i = 1; i < f.length; i++) if (f[i] > f[peak]) peak = i;
                const d = buf.getChannelData(0);
                let energy = 0;
                for (let i = 0; i < 2048; i++) energy += d[i] * d[i];
                return { peak: peak, energy: energy };
            });
        }
        Promise.all([measure(1000), measure(5000)]).then(function (r) {
            // The spectrum's peak bin must track the oscillator: 1 kHz and 5 kHz land in different
            // bins, and both must be where a 2048-point FFT at 44.1 kHz puts them (~46 and ~232).
            print("peaks=" + r[0].peak + "," + r[1].peak);
            // And the samples must carry energy for both -- an analyser is a pass-through, so what
            // the spectrum saw is what reached the destination.
            print("energy=" + (r[0].energy > 100) + "," + (r[1].energy > 100));
        });
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "peaks=46,232",
        "energy=true,true",
    ]);
}

/// The CONSTRUCTOR form has to build the same node the factory does, and a constructor OPTION has to
/// reach the DSP.
///
/// This is the audio form of the bar 3.4 was held to: an `AudioParam` stored and never read is the
/// metadata-only `getParameter` under another name. The options dictionary is applied by writing the
/// node's own public members, so an option travels by exactly the route a page's own assignment
/// does — and the assertion is not that the value reads back, which a stored copy satisfies, but
/// that the SAMPLES move.
#[tokio::test]
async fn a_constructor_option_reaches_the_dsp() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        // A checksum over the raw float bits, not a sum: two signals with the same total energy and
        // different shapes have to come out different.
        function fnv(d) {
            const b = new Uint8Array(d.buffer.slice(0));
            let h = 0x811c9dc5;
            for (let i = 0; i < b.length; i++) { h ^= b[i]; h = (h * 0x01000193) >>> 0; }
            return h >>> 0;
        }
        function render(build) {
            const ctx = new OfflineAudioContext(1, 8192, 44100);
            build(ctx).connect(ctx.destination);
            return ctx.startRendering();
        }
        // 1. The constructor and the factory must produce the SAME node: same defaults, same
        //    surface, same samples. A difference between the two routes is a one-expression tell.
        const viaCtor = render(function (c) { const o = new OscillatorNode(c); o.start(0); return o; });
        const viaFactory = render(function (c) { const o = c.createOscillator(); o.start(0); return o; });
        // 2. `frequency` set through the OPTIONS DICTIONARY must move the samples, not just read
        //    back. 440 (the default) against 880 through the same constructor.
        const opt880 = render(function (c) { const o = new OscillatorNode(c, { frequency: 880 }); o.start(0); return o; });
        // 3. And a non-param option (`type`) likewise: a square wave is not a sine.
        const optSquare = render(function (c) { const o = new OscillatorNode(c, { type: "square" }); o.start(0); return o; });
        // 4. A gain of 0.25 in the dictionary must scale the output by 0.25.
        const gained = render(function (c) {
            const o = new OscillatorNode(c, { frequency: 440 });
            const g = new GainNode(c, { gain: 0.25 });
            o.connect(g); o.start(0); return g;
        });
        Promise.all([viaCtor, viaFactory, opt880, optSquare, gained]).then(function (r) {
            const d = r.map(function (b) { return b.getChannelData(0); });
            print("routes-agree=" + (fnv(d[0]) === fnv(d[1])));
            print("freq-option-moves=" + (fnv(d[0]) !== fnv(d[2])));
            print("type-option-moves=" + (fnv(d[0]) !== fnv(d[3])));
            let dry = 0, wet = 0;
            for (let i = 1000; i < 5000; i++) { dry += Math.abs(d[0][i]); wet += Math.abs(d[4][i]); }
            print("gain-option=" + (wet / dry).toFixed(3));
            // The samples are a real 440 Hz sine: it crosses zero 880 times per second, and 8192
            // frames at 44.1 kHz is 0.1857 s of it.
            let zc = 0;
            for (let i = 1; i < d[0].length; i++) if ((d[0][i - 1] < 0) !== (d[0][i] < 0)) zc++;
            print("zero-crossings=" + zc);
        });
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "routes-agree=true",
        "freq-option-moves=true",
        "type-option-moves=true",
        "gain-option=0.250",
        "zero-crossings=163",
    ]);
}

/// A silent analyser answers `-Infinity` in every bin, on the live path AND after a full render.
///
/// The no-noise-lie rule. `20·log10(0)` is `-Infinity` and that IS the answer for a bin with no
/// energy; the tape reported -170.02, a plausible noise floor that was not there, and returning
/// small random values to look plausible is the specific fake this test exists to catch. It is
/// asserted here as well as in `the_three_tape_detections_are_gone` because that test's analyser is
/// merely fresh, while this one has been through a whole render with nothing connected upstream —
/// the case where a DSP that accumulated any garbage at all would show it.
#[tokio::test]
async fn a_silent_analyser_is_all_negative_infinity() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.eval("a", r#"
        function allNegInf(f) {
            for (let i = 0; i < f.length; i++) if (f[i] !== -Infinity) return "bin " + i + " = " + f[i];
            return "true";
        }
        const ac = new AudioContext();
        const idle = ac.createAnalyser();
        const f0 = new Float32Array(idle.frequencyBinCount);
        idle.getFloatFrequencyData(f0);
        print("live=" + allNegInf(f0) + "," + f0.length);

        // Rendered, connected DOWNSTREAM to the destination, with nothing connected upstream.
        const oc = new OfflineAudioContext(1, 8192, 44100);
        const an = oc.createAnalyser();
        an.connect(oc.destination);
        oc.startRendering().then(function (buf) {
            const f = new Float32Array(an.frequencyBinCount);
            an.getFloatFrequencyData(f);
            print("rendered=" + allNegInf(f));
            // The byte form of the same silence is 0 everywhere, and the time-domain byte form is
            // 128 -- the zero of an unsigned 8-bit waveform, not 0. A fabricated buffer gets that
            // one wrong in the opposite direction.
            const u = new Uint8Array(an.frequencyBinCount);
            an.getByteFrequencyData(u);
            const t = new Uint8Array(an.fftSize);
            an.getByteTimeDomainData(t);
            print("bytes=" + u[0] + "," + u[u.length - 1] + "," + t[0] + "," + t[t.length - 1]);
            // And the destination it fed is silent too, which is what makes the -Infinity honest
            // rather than a special case inside the analyser.
            const d = buf.getChannelData(0);
            let peak = 0;
            for (let i = 0; i < d.length; i++) peak = Math.max(peak, Math.abs(d[i]));
            print("destination-peak=" + peak);
        });
    "#.into()).unwrap();
    e.run().await.unwrap();
    assert_eq!(e.take_output(), vec![
        "live=true,1024",
        "rendered=true",
        "bytes=0,0,128,128",
        "destination-peak=0",
    ]);
}
