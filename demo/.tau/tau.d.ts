// Ambient declaration for the `tau` virtual module emitted by .tau/vite.config.js.
// Keep in sync with TAU_SOURCE in vite.config.js.
declare module 'tau' {
  export * from '@react-three/fiber';
  export * as haptics from '@tauri-apps/plugin-haptics';
  export * as notification from '@tauri-apps/plugin-notification';
  export * as dialog from '@tauri-apps/plugin-dialog';
  export * as fs from '@tauri-apps/plugin-fs';
}
