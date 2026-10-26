(function () {
  // document.createEvent over every Event interface Chrome 150 exposes, Blink's aliases, the removed and
  // deprecated names, each in three casings; then the argument conversions.
  var rows = [];
  // Chrome 150's own Event interfaces on this page (its `Object.getOwnPropertyNames(window)` sweep), Blink's
  // aliases and a few names Blink once had, fixed here so the rows depend on createEvent alone
  var names = ["", "AnimationEvent", "AnimationPlaybackEvent", "AudioProcessingEvent", "BeforeInstallPromptEvent", "BeforeUnloadEvent", "BeforeUnloadEvents", "BlobEvent", "CharacterBoundsUpdateEvent", "ClipboardChangeEvent", "ClipboardEvent", "CloseEvent", "CommandEvent", "CompositionEvent", "CompositionEvents", "ContentVisibilityAutoStateChangeEvent", "CookieChangeEvent", "CustomEvent", "CustomEvents", "DeviceMotionEvent", "DeviceMotionEvents", "DeviceOrientationEvent", "DocumentPictureInPictureEvent", "DragEvent", "DragEvents", "ErrorEvent", "Event", "Events", "FocusEvent", "FocusEvents", "FontFaceSetLoadEvent", "FormDataEvent", "GPUUncapturedErrorEvent", "GamepadEvent", "HIDConnectionEvent", "HIDInputReportEvent", "HTMLEvents", "HashChangeEvent", "HashChangeEvents", "IDBVersionChangeEvent", "InputEvent", "InterestEvent", "KeyboardEvent", "KeyboardEvents", "MIDIConnectionEvent", "MIDIMessageEvent", "MediaEncryptedEvent", "MediaKeyMessageEvent", "MediaQueryListEvent", "MediaStreamEvent", "MediaStreamTrackEvent", "MessageEvent", "MessageEvents", "MouseEvent", "MouseEvents", "MutationEvent", "MutationEvents", "NavigateEvent", "NavigationCurrentEntryChangeEvent", "OfflineAudioCompletionEvent", "OrientationEvent", "PageRevealEvent", "PageSwapEvent", "PageTransitionEvent", "PaymentMethodChangeEvent", "PaymentRequestUpdateEvent", "PictureInPictureEvent", "PointerEvent", "PopStateEvent", "PresentationConnectionAvailableEvent", "PresentationConnectionCloseEvent", "ProgressEvent", "PromiseRejectionEvent", "RTCDTMFToneChangeEvent", "RTCDataChannelEvent", "RTCErrorEvent", "RTCPeerConnectionIceErrorEvent", "RTCPeerConnectionIceEvent", "RTCTrackEvent", "SVGEvents", "SVGZoomEvents", "SecurityPolicyViolationEvent", "SensorErrorEvent", "SnapEvent", "SpeechRecognitionErrorEvent", "SpeechRecognitionEvent", "SpeechSynthesisErrorEvent", "SpeechSynthesisEvent", "StorageEvent", "StorageEvents", "SubmitEvent", "TaskPriorityChangeEvent", "TextEvent", "TextEvents", "TextFormatUpdateEvent", "TextUpdateEvent", "ToggleEvent", "TouchEvent", "TouchEvents", "TrackEvent", "TransitionEvent", "UIEvent", "UIEvents", "USBConnectionEvent", "VirtualKeyboardGeometryChangeEvent", "WebGLContextEvent", "WebKitAnimationEvent", "WebKitTransitionEvent", "WheelEvent", "WindowControlsOverlayGeometryChangeEvent", "XMLHttpRequestProgressEvent", "XRInputSourceEvent", "XRInputSourcesChangeEvent", "XRLayerEvent", "XRReferenceSpaceEvent", "XRSessionEvent", "XRVisibilityMaskChangeEvent", "webkitSpeechRecognitionError", "webkitSpeechRecognitionEvent"];
  names.sort();
  function one(label, arg) {
    try {
      var e = document.createEvent(arg);
      var init;
      try { document.dispatchEvent(e); init = 'dispatched'; } catch (x) { init = x.name; }
      rows.push([label, 'ok', Object.prototype.toString.call(e), Object.getPrototypeOf(e) === (window[Object.prototype.toString.call(e).slice(8, -1)] || {}).prototype,
        JSON.stringify(e.type), e.bubbles, e.cancelable, e.isTrusted, init]);
    } catch (x) { rows.push([label, 'throw', x.name, x.message]); }
  }
  names.forEach(function (n) {
    one(n, n);
    if (n.toLowerCase() !== n) one(n + ' (lower)', n.toLowerCase());
    if (n.toUpperCase() !== n) one(n + ' (upper)', n.toUpperCase());
  });
  try { document.createEvent(); rows.push(['no argument', 'ok']); } catch (x) { rows.push(['no argument', 'throw', x.name, x.message]); }
  one('undefined', undefined);
  one('null', null);
  one('object with toString', { toString: function () { return 'mouseevent'; } });
  try { document.createEvent(Symbol('x')); rows.push(['symbol', 'ok']); } catch (x) { rows.push(['symbol', 'throw', x.name, x.message]); }
  try { Document.prototype.createEvent.call({}, 'Event'); rows.push(['foreign receiver', 'ok']); } catch (x) { rows.push(['foreign receiver', 'throw', x.name, x.message]); }
  // a frame's document makes the frame's events
  var f = document.createElement('iframe'); document.body.appendChild(f);
  try { var fe = f.contentDocument.createEvent('MouseEvents'); rows.push(['frame realm', fe instanceof f.contentWindow.MouseEvent, fe instanceof MouseEvent]); } catch (x) { rows.push(['frame realm', 'throw', x.name, x.message]); }
  try { f.contentDocument.createEvent('TouchEvent'); rows.push(['frame TouchEvent', 'ok']); } catch (x) { rows.push(['frame TouchEvent', 'throw', x.name, x.message]); }
  return rows;
})()
