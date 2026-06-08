// @ts-check
import { defineConfig } from "astro/config";
import tailwindcss from "@tailwindcss/vite";
import remarkDirective from "remark-directive";
import remarkCallouts from "./src/lib/remark-callouts.mjs";

// https://astro.build/config
export default defineConfig({
  site: "https://github.com/hyperterse/seaport",
  prefetch: {
    prefetchAll: true,
    defaultStrategy: "hover",
  },
  markdown: {
    remarkPlugins: [remarkDirective, remarkCallouts],
    shikiConfig: {
      theme: "github-dark",
      wrap: false,
    },
  },
  vite: {
    plugins: [tailwindcss()],
  },
});
