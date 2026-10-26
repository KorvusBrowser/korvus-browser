const P = ReadableStream.prototype, d = Object.getOwnPropertyDescriptor(P, Symbol.asyncIterator);
postMessage([P[Symbol.asyncIterator] === P.values, d.value.name, d.value.length, d.writable, d.enumerable, d.configurable].join());
