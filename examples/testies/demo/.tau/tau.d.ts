// Ambient declaration for the `tau` virtual module emitted by .tau/vite.config.js.
// The Vite plugin re-exports the four Tauri plugin namespaces — keep this in sync
// with the TAU_SOURCE constant there.
declare module 'tau' {
  export * as haptics from '@tauri-apps/plugin-haptics';
  export * as notification from '@tauri-apps/plugin-notification';
  export * as dialog from '@tauri-apps/plugin-dialog';
  export * as fs from '@tauri-apps/plugin-fs';
}
