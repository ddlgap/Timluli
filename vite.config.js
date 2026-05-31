import { defineConfig } from 'vite';
import { resolve } from 'path';

const root = resolve(__dirname, 'src');

export default defineConfig({
  root,
  publicDir: resolve(__dirname, 'public'),
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: '127.0.0.1',
  },
  build: {
    outDir: resolve(__dirname, 'dist'),
    emptyOutDir: true,
    target: 'esnext',
    minify: false,
    sourcemap: false,
    rollupOptions: {
      input: {
        main: resolve(root, 'index.html'),
        mic: resolve(root, 'mic.html'),
        panel: resolve(root, 'panel.html'),
        speech: resolve(root, 'speech.html'),
        settings: resolve(root, 'settings.html'),
        onboarding: resolve(root, 'onboarding.html'),
      },
    },
  },
});
