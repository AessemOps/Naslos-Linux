import { sveltekit } from '@sveltejs/kit/vite';
import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';
import pkg from './package.json' with { type: 'json' };

export default defineConfig({
  plugins: [
    sveltekit({
      preprocess: vitePreprocess(),
      adapter: adapter({
        pages: 'dist',
        assets: 'dist',
        fallback: 'index.html',
        precompress: false,
        strict: false
      }),
      paths: {
        base: ''
      },
      // SvelteKit 3 removed the built-in $lib module; keep the existing
      // imports working instead of rewriting the whole tree to #lib.
      alias: {
        $lib: 'src/lib'
      }
    })
  ],
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version)
  },
  build: {
    // noVNC 1.7 uses top-level await, so the default es2020 target refuses it.
    target: 'es2022'
  },
  server: {
    proxy: {
      '/api': {
        target: 'http://localhost:8080',
        changeOrigin: true
      }
    }
  }
});
