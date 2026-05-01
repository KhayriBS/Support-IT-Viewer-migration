# Tauri + SvelteKit + TypeScript

This template should help get you started developing with Tauri, SvelteKit and TypeScript in Vite.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).

## Capture Backend Configuration

You can tune the screen-capture backend from environment variables:

- `LUMIERE_CAPTURE_BACKEND=auto|dxgi|wgc`
- `LUMIERE_CAPTURE_MONITOR_INDEX=<0-based index>`

Notes:

- `auto` tries DXGI first, then WGC.
- `LUMIERE_CAPTURE_MONITOR_INDEX` selects the display index used by capture backends.
- In WGC mode, monitor index `0` uses native WinRT capture path; non-zero indexes use the fallback path to ensure deterministic monitor targeting.
