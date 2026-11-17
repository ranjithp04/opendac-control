# OpenDAC Control app (M2)

Tauri 2 desktop app (window + Dock icon + menu bar icon): `src-tauri` (Rust shell on `g6-core`) and `ui` (Svelte + TypeScript + Vite). The UI <-> Rust contract is [API.md](API.md).

```sh
cd app/ui
npm install
npm run check          # svelte-check
npm run build          # writes app/ui/dist
npm run tauri -- build --debug --no-bundle   # CLI is a dev dependency, no global install
```

`cargo build -p opendac-control` (or `--workspace`) works without Node: `build.rs` creates an empty `app/ui/dist` if the UI was not built. The app only gets a UI after `npm run build`.

Running the app opens the G6 (`npm run tauri -- dev`); do that deliberately.

`npm run dev` in a browser (http://localhost:1420) runs the UI against an in-memory mock backend (`ui/src/lib/mock.ts`), no Tauri and no device. Console handle: `__g6mock.switchOutput()`, `.disconnect()`, `.connect()`, `.pressScout()`, `.pressSbx()`, `.failNext('deviceTimeout')`; `?mock=disconnected` starts disconnected.

Inter and JetBrains Mono are bundled from the `@fontsource` packages (latin subset), so the app needs no network and the CSP stays closed.
