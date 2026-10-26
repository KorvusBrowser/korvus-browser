//! `speech` Web-API interfaces (blueprint domain, the Web Speech API). One file per interface.
//!
//! Korvus has no TTS engine -- no audio pipeline and no installed voice list -- so `SpeechSynthesis`
//! is a real interface with a real shape whose four ACTIONS (`speak`/`cancel`/`pause`/`resume`) are
//! honest D2 stub rows, while its four read members answer what a browser with no voice pack answers.
//! `SpeechSynthesisUtterance` is different: it is pure spec-defined state that a page can set and read
//! back without anything ever being spoken, so it is implementable in full.

pub(crate) mod speech_synthesis;
pub(crate) mod speech_synthesis_utterance;
