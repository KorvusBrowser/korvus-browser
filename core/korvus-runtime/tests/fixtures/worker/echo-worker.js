// GOAL-4 W1.0 smoke worker: echo `data + 1` back to the page so the harness can assert
// bidirectional postMessage. Must behave identically under the same-isolate sim (W1.0) and the
// real isolate (W1.1+).
self.onmessage = (e) => { self.postMessage(e.data + 1); };
