importScripts('probe.js');
postMessage(probe(['name', 'onmessage', 'onmessageerror', 'onrtctransform'], [
  ['close', null], ['postMessage', null], ['requestAnimationFrame', [[]]], ['cancelAnimationFrame', [[]]],
  ['webkitRequestFileSystem', [[], [0], [0, 0]]], ['webkitRequestFileSystemSync', [[], [0]]],
  ['webkitResolveLocalFileSystemSyncURL', [[]]], ['webkitResolveLocalFileSystemURL', [[], ['x'], ['x', () => {}]]],
]));
