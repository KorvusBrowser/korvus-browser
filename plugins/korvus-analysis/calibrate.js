/* korvus-analysis calibration: a loop of N plain script calls. core/korvus-webapi (analysis_hook.rs,
   calibrate_hook_exits) runs it in a private context, once with V8's post-call hooks paused and once with
   them on, to measure what one hook early exit costs (goal T). Analysis tooling, not the kernel's: it
   lives here, outside the kernel's JS-string inventory, and runs only in an analysis-v8 build. __N__ and
   __TAG__ are filled in per run (a source of its own each time: no compilation cache crosses variants). */
(function () { function f(x) { return x; } let s = 0; for (let i = 0; i < __N__; i++) s = f(s); return s; })() /* __TAG__ */
