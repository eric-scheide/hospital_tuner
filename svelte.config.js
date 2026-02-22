import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

export default {
  // vitePreprocess enables use of <style lang="scss">, TypeScript in <script>,
  // and other preprocessors supported by Vite.
  // For plain CSS + JS (v1), this is a no-op but satisfies the plugin's config
  // resolution: @sveltejs/vite-plugin-svelte looks for this file at startup.
  preprocess: vitePreprocess(),
};
