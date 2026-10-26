// @korvus/client: a browser front end over the DevTools protocol, as web components and a headless core.
//
//   <script type="module" src="korvus-client.js"></script>
//   <korvus-browser endpoint="ws://127.0.0.1:9222/?ticket=…"></korvus-browser>

import { KorvusBrowser } from "./browser.js";
import { KorvusView } from "./view.js";

export { Client } from "./client.js";
export { Connection, CdpError, type CdpEvent } from "./cdp.js";
export { Tab, Mod, type TargetInfo, type HistoryEntry, type Dialog, type Frame } from "./tab.js";
export { KorvusBrowser, addressFor } from "./browser.js";
export { KorvusView } from "./view.js";

if (!customElements.get("korvus-view")) customElements.define("korvus-view", KorvusView);
if (!customElements.get("korvus-browser")) customElements.define("korvus-browser", KorvusBrowser);
