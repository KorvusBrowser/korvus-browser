(async () => {
  // The editing commands in the states a page reaches: a range over non-editable text, a caret and a range in a
  // contenteditable host (bold, a list, a heading, a coloured span), a focused textarea, an XML document and a
  // frameless HTML document; and what execCommand does that needs no editing.
  const R = [];
  const names = ["alignCenter", "alignJustified", "alignLeft", "alignRight", "backColor", "backwardDelete", "bold", "copy", "createLink", "cut", "defaultParagraphSeparator", "delete", "deleteBackward", "deleteBackwardByDecomposingPreviousCharacter", "deleteForward", "deleteToBeginningOfLine", "deleteToBeginningOfParagraph", "deleteToEndOfLine", "deleteToEndOfParagraph", "deleteToMark", "deleteWordBackward", "deleteWordForward", "findString", "fontName", "fontSize", "fontSizeDelta", "foreColor", "formatBlock", "forwardDelete", "hiliteColor", "ignoreSpelling", "indent", "insertBacktab", "insertHorizontalRule", "insertHTML", "insertImage", "insertLineBreak", "insertNewline", "insertNewlineInQuotedContent", "insertOrderedList", "insertParagraph", "insertTab", "insertText", "insertUnorderedList", "italic", "justifyCenter", "justifyFull", "justifyLeft", "justifyNone", "justifyRight", "makeTextWritingDirectionLeftToRight", "makeTextWritingDirectionNatural", "makeTextWritingDirectionRightToLeft", "moveBackward", "moveBackwardAndModifySelection", "moveDown", "moveDownAndModifySelection", "moveForward", "moveForwardAndModifySelection", "moveLeft", "moveLeftAndModifySelection", "movePageDown", "movePageDownAndModifySelection", "movePageUp", "movePageUpAndModifySelection", "moveParagraphBackward", "moveParagraphBackwardAndModifySelection", "moveParagraphForward", "moveParagraphForwardAndModifySelection", "moveRight", "moveRightAndModifySelection", "moveToBeginningOfDocument", "moveToBeginningOfDocumentAndModifySelection", "moveToBeginningOfLine", "moveToBeginningOfLineAndModifySelection", "moveToBeginningOfParagraph", "moveToBeginningOfParagraphAndModifySelection", "moveToBeginningOfSentence", "moveToBeginningOfSentenceAndModifySelection", "moveToEndOfDocument", "moveToEndOfDocumentAndModifySelection", "moveToEndOfLine", "moveToEndOfLineAndModifySelection", "moveToEndOfParagraph", "moveToEndOfParagraphAndModifySelection", "moveToEndOfSentence", "moveToEndOfSentenceAndModifySelection", "moveToLeftEndOfLine", "moveToLeftEndOfLineAndModifySelection", "moveToRightEndOfLine", "moveToRightEndOfLineAndModifySelection", "moveUp", "moveUpAndModifySelection", "moveWordBackward", "moveWordBackwardAndModifySelection", "moveWordForward", "moveWordForwardAndModifySelection", "moveWordLeft", "moveWordLeftAndModifySelection", "moveWordRight", "moveWordRightAndModifySelection", "outdent", "overWrite", "paste", "pasteAndMatchStyle", "pasteFromImageURL", "pasteGlobalSelection", "print", "redo", "removeFormat", "scrollLineDown", "scrollLineUp", "scrollPageBackward", "scrollPageForward", "scrollToBeginningOfDocument", "scrollToEndOfDocument", "selectAll", "selectLine", "selectParagraph", "selectSentence", "selectToMark", "selectWord", "setMark", "strikethrough", "styleWithCss", "subscript", "superscript", "swapWithMark", "toggleBold", "toggleItalic", "toggleUnderline", "transpose", "underline", "undo", "unlink", "unscript", "unselect", "useCss", "yank", "yankAndSelect"];
  const q = (f, n, d = document) => { try { return d[f](n); } catch (e) { return 'throw:' + e.name + ': ' + e.message; } };
  const all = (label, list) => {
    const en = [], st = [], va = [], ind = [];
    for (const n of list) {
      if (q('queryCommandEnabled', n) === true) en.push(n);
      if (q('queryCommandState', n) === true) st.push(n);
      const v = q('queryCommandValue', n); if (v !== '' && v !== 'false') va.push(n + '=' + v);
      if (q('queryCommandIndeterm', n) === true) ind.push(n);
    }
    R.push([label, 'enabled', en.join(',')], [label, 'state', st.join(',')], [label, 'value', va.join(',')], [label, 'indeterm', ind.join(',')]);
  };
  const sel = getSelection();
  const p = document.createElement('p'); p.textContent = 'plain words here'; document.body.append(p);
  sel.selectAllChildren(p);
  all('range in non-editable', names);
  sel.collapse(p.firstChild, 3);
  all('caret in non-editable', names);
  const ed = document.createElement('div'); ed.contentEditable = 'true';
  ed.innerHTML = '<b>bold</b> plain <i>it</i><ul><li>item</li></ul><h2>head</h2><p style="text-align:center"><span style="color: rgb(1, 2, 3); font-family: Arial; font-size: 24px; background-color: rgb(9, 8, 7)">styled</span></p>';
  document.body.append(ed); ed.focus();
  const at = (node, off) => { sel.collapse(node, off); };
  at(ed.querySelector('b').firstChild, 2); all('caret in bold', names);
  at(ed.childNodes[1], 2); all('caret in plain', names);
  at(ed.querySelector('li').firstChild, 1); all('caret in list item', names);
  at(ed.querySelector('h2').firstChild, 1); all('caret in heading', names);
  at(ed.querySelector('span').firstChild, 2); all('caret in styled span', names);
  sel.setBaseAndExtent(ed.querySelector('b').firstChild, 1, ed.childNodes[1], 3); all('range bold to plain', names);
  sel.selectAllChildren(ed); all('range whole host', names);
  const ta = document.createElement('textarea'); ta.value = 'some text'; document.body.append(ta); ta.focus(); ta.setSelectionRange(2, 2);
  all('caret in textarea', names);
  ta.setSelectionRange(1, 5); all('range in textarea', names);
  ta.blur(); ed.blur(); sel.removeAllRanges();
  all('no selection', names);
  // execCommand that needs no editing
  const ex = (n, v) => { let r; try { r = document.execCommand(n, false, v); } catch (e) { r = 'throw:' + e.name + ': ' + e.message; } return r; };
  R.push(['exec selectAll', ex('selectAll'), sel.type, sel.anchorNode && sel.anchorNode.nodeName, sel.focusNode && sel.focusNode.nodeName]);
  R.push(['exec unselect', ex('unselect'), sel.type]);
  R.push(['exec unselect again', ex('unselect'), sel.type]);
  R.push(['exec styleWithCSS true', ex('styleWithCSS', 'true'), q('queryCommandState', 'styleWithCSS'), q('queryCommandValue', 'styleWithCSS')]);
  R.push(['exec styleWithCSS false', ex('styleWithCSS', 'false'), q('queryCommandState', 'styleWithCSS')]);
  R.push(['exec useCSS false', ex('useCSS', 'false'), q('queryCommandState', 'styleWithCSS'), q('queryCommandState', 'useCSS')]);
  R.push(['exec useCSS true', ex('useCSS', 'true'), q('queryCommandState', 'styleWithCSS')]);
  R.push(['exec defaultParagraphSeparator P', ex('defaultParagraphSeparator', 'P'), q('queryCommandValue', 'defaultParagraphSeparator')]);
  R.push(['exec defaultParagraphSeparator span', ex('defaultParagraphSeparator', 'span'), q('queryCommandValue', 'defaultParagraphSeparator')]);
  R.push(['exec defaultParagraphSeparator div', ex('defaultParagraphSeparator', 'div'), q('queryCommandValue', 'defaultParagraphSeparator')]);
  R.push(['exec findString', ex('findString', 'words'), sel.type, String(sel)]);
  sel.removeAllRanges();
  R.push(['exec findString missing', ex('findString', 'zzzqqq'), sel.type]);
  R.push(['exec bold non-editable', ex('bold'), ed.innerHTML.length]);
  R.push(['exec insertText no focus', ex('insertText', 'x')]);
  // documents without a frame, and XML
  const xml = new DOMParser().parseFromString('<a/>', 'application/xml');
  for (const f of ['execCommand', 'queryCommandEnabled', 'queryCommandIndeterm', 'queryCommandState', 'queryCommandSupported', 'queryCommandValue']) R.push(['xml ' + f, q(f, 'bold', xml)]);
  const html = new DOMParser().parseFromString('<p>x</p>', 'text/html');
  for (const f of ['execCommand', 'queryCommandEnabled', 'queryCommandSupported', 'queryCommandValue', 'queryCommandState']) R.push(['frameless ' + f, q(f, 'selectAll', html)]);
  const impl = document.implementation.createHTMLDocument('t');
  R.push(['createHTMLDocument supported', q('queryCommandSupported', 'bold', impl)]);
  ed.remove(); ta.remove(); p.remove();
  return R;
})()
