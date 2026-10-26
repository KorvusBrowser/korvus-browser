// One ES module, `build/korvus-client.js`, with the stand-alone page (`static/index.html`) beside it. korvus serves
// both from its own port (`/client/`); `build/` is committed so the engine's build needs no Node.
import { defineConfig } from "vite";

export default defineConfig({
  publicDir: "static",
  build: {
    outDir: "build",
    emptyOutDir: true,
    target: "es2022",
    lib: { entry: "src/index.ts", formats: ["es"], fileName: () => "korvus-client.js" },
  },
});
