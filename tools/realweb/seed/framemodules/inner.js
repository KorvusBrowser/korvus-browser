export function load() { return import("./b.js").then(function (m) { return "inner:" + m.b; }); }
