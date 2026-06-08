# Seaport landing page

Marketing landing page for [Seaport](https://github.com/hyperterse/seaport),
built with [Astro](https://astro.build) and [Tailwind CSS v4](https://tailwindcss.com),
managed with [Bun](https://bun.sh).

## Commands

All commands are run from this directory:

| Command         | Action                                       |
| --------------- | -------------------------------------------- |
| `bun install`   | Install dependencies                         |
| `bun run dev`   | Start the dev server at `localhost:4321`     |
| `bun run build` | Build the production site to `./dist/`       |
| `bun run preview` | Preview the production build locally       |

## Structure

```text
landing/
├── public/              static assets (favicon)
├── src/
│   ├── components/      Nav, Hero, Features, Workflow, Sandbox, Output, Install, Footer
│   ├── layouts/         Layout.astro (head, fonts, global styles)
│   ├── pages/           index.astro
│   └── styles/          global.css (Tailwind import + @theme tokens)
├── astro.config.mjs     Astro config, wires the Tailwind Vite plugin
└── package.json
```

Tailwind is loaded via the `@tailwindcss/vite` plugin (configured in
`astro.config.mjs`) and `@import "tailwindcss"` in `src/styles/global.css`.
The custom color palette and fonts live in the `@theme` block of that file.
