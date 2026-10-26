(async () => {
  // document.queryCommand* and execCommand on a page with no editable content and no selection, for every name Blink
  // knows (editor_command_names.h) and a few it does not; then sendBeacon's resource entry and what it reads, and
  // DOMParser's type check.
  const R = [];
  const names = ["alignCenter", "alignJustified", "alignLeft", "alignRight", "backColor", "backwardDelete", "bold", "copy", "createLink", "cut", "defaultParagraphSeparator", "delete", "deleteBackward", "deleteBackwardByDecomposingPreviousCharacter", "deleteForward", "deleteToBeginningOfLine", "deleteToBeginningOfParagraph", "deleteToEndOfLine", "deleteToEndOfParagraph", "deleteToMark", "deleteWordBackward", "deleteWordForward", "findString", "fontName", "fontSize", "fontSizeDelta", "foreColor", "formatBlock", "forwardDelete", "hiliteColor", "ignoreSpelling", "indent", "insertBacktab", "insertHorizontalRule", "insertHTML", "insertImage", "insertLineBreak", "insertNewline", "insertNewlineInQuotedContent", "insertOrderedList", "insertParagraph", "insertTab", "insertText", "insertUnorderedList", "italic", "justifyCenter", "justifyFull", "justifyLeft", "justifyNone", "justifyRight", "makeTextWritingDirectionLeftToRight", "makeTextWritingDirectionNatural", "makeTextWritingDirectionRightToLeft", "moveBackward", "moveBackwardAndModifySelection", "moveDown", "moveDownAndModifySelection", "moveForward", "moveForwardAndModifySelection", "moveLeft", "moveLeftAndModifySelection", "movePageDown", "movePageDownAndModifySelection", "movePageUp", "movePageUpAndModifySelection", "moveParagraphBackward", "moveParagraphBackwardAndModifySelection", "moveParagraphForward", "moveParagraphForwardAndModifySelection", "moveRight", "moveRightAndModifySelection", "moveToBeginningOfDocument", "moveToBeginningOfDocumentAndModifySelection", "moveToBeginningOfLine", "moveToBeginningOfLineAndModifySelection", "moveToBeginningOfParagraph", "moveToBeginningOfParagraphAndModifySelection", "moveToBeginningOfSentence", "moveToBeginningOfSentenceAndModifySelection", "moveToEndOfDocument", "moveToEndOfDocumentAndModifySelection", "moveToEndOfLine", "moveToEndOfLineAndModifySelection", "moveToEndOfParagraph", "moveToEndOfParagraphAndModifySelection", "moveToEndOfSentence", "moveToEndOfSentenceAndModifySelection", "moveToLeftEndOfLine", "moveToLeftEndOfLineAndModifySelection", "moveToRightEndOfLine", "moveToRightEndOfLineAndModifySelection", "moveUp", "moveUpAndModifySelection", "moveWordBackward", "moveWordBackwardAndModifySelection", "moveWordForward", "moveWordForwardAndModifySelection", "moveWordLeft", "moveWordLeftAndModifySelection", "moveWordRight", "moveWordRightAndModifySelection", "outdent", "overWrite", "paste", "pasteAndMatchStyle", "pasteFromImageURL", "pasteGlobalSelection", "print", "redo", "removeFormat", "scrollLineDown", "scrollLineUp", "scrollPageBackward", "scrollPageForward", "scrollToBeginningOfDocument", "scrollToEndOfDocument", "selectAll", "selectLine", "selectParagraph", "selectSentence", "selectToMark", "selectWord", "setMark", "strikethrough", "styleWithCss", "subscript", "superscript", "swapWithMark", "toggleBold", "toggleItalic", "toggleUnderline", "transpose", "underline", "undo", "unlink", "unscript", "unselect", "useCss", "yank", "yankAndSelect", "nonsense", "BOLD", "Bold", "styleWithCSS", "useCSS", "enableObjectResizing", "enableInlineTableEditing", "contentReadOnly", "heading", "decreaseFontSize", "increaseFontSize", "insertBrOnReturn", "readOnly", "backColor", "hiliteColor", ""];
  const q = (f, n) => { try { return document[f](n); } catch (e) { return 'throw:' + e.name + ': ' + e.message; } };
  for (const n of names) R.push([n, q('queryCommandSupported', n), q('queryCommandEnabled', n), q('queryCommandState', n), q('queryCommandValue', n), q('queryCommandIndeterm', n)]);
  for (const n of ['bold', 'insertText', 'copy', 'undo', 'styleWithCSS', 'defaultParagraphSeparator', 'removeFormat', 'nonsense', 'selectAll', 'unselect']) {
    let r; try { r = document.execCommand(n, false, n === 'defaultParagraphSeparator' ? 'p' : 'x'); } catch (e) { r = 'throw:' + e.name + ': ' + e.message; }
    R.push(['exec ' + n, r, q('queryCommandValue', n), q('queryCommandState', n), String(getSelection())]);
  }
  try { document.execCommand(); } catch (e) { R.push(['exec no args', 'throw:' + e.name + ': ' + e.message]); }
  try { document.queryCommandSupported(); } catch (e) { R.push(['supported no args', 'throw:' + e.name + ': ' + e.message]); }
  // sendBeacon: what it reads of the page, and its timeline entry
  const reads = [];
  const bd = Object.getOwnPropertyDescriptor(Node.prototype, 'baseURI');
  Object.defineProperty(Node.prototype, 'baseURI', { configurable: true, get() { reads.push('baseURI'); return bd.get.call(this); } });
  const hd = Object.getOwnPropertyDescriptor(Location.prototype, 'href') || Object.getOwnPropertyDescriptor(location, 'href');
  const td = Object.getOwnPropertyDescriptor(Blob.prototype, 'type');
  Object.defineProperty(Blob.prototype, 'type', { configurable: true, get() { reads.push('Blob.type'); return td.get.call(this); } });
  R.push(['beacon string', navigator.sendBeacon('/beacon1', 'x')]);
  R.push(['beacon blob json', navigator.sendBeacon('/beacon2', new Blob(['{}'], { type: 'application/json' }))]);
  R.push(['beacon blob text', navigator.sendBeacon('/beacon3', new Blob(['x'], { type: 'text/plain' }))]);
  Object.defineProperty(Node.prototype, 'baseURI', bd);
  Object.defineProperty(Blob.prototype, 'type', td);
  R.push(['beacon reads', reads.join(',')]);
  await new Promise((r) => setTimeout(r, 600));
  for (const e of performance.getEntriesByType('resource').filter((e) => /beacon/.test(e.name))) R.push(['beacon entry ' + e.name.replace(/^.*\//, ''), e.initiatorType, e.responseStatus, e.renderBlockingStatus, e.deliveryType, e.transferSize > 0]);
  // DOMParser's SupportedType
  for (const t of ['text/html', 'text/xml', 'application/xml', 'application/xhtml+xml', 'image/svg+xml', 'text/foo', 'TEXT/HTML', '']) {
    let r; try { r = new DOMParser().parseFromString('<a/>', t).documentElement.nodeName; } catch (e) { r = 'throw:' + e.name + ': ' + e.message; }
    R.push(['DOMParser ' + t, r]);
  }
  return R;
})()
