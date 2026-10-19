# site

The download page for Terminal. It is one static page built with SvelteKit for Vercel, plus a
`/download` route that redirects to the latest release DMG.

This folder is its own npm project, separate from the Cargo workspace. Run the commands below from
`site/`, and set the Vercel project's Root Directory to `site`. It needs Node 22.17 or later, the
minimum SvelteKit supports; `.npmrc` turns on `engine-strict`, so `npm install` stops on older
versions.

```sh
cd site
npm install
npm run dev       # http://localhost:5173
npm run build     # Vercel output in .vercel/output
npm run preview   # serve the production build on http://localhost:4173
npm run check
```

## How it fits together

- `src/routes/+page.svelte` is the page. It is prerendered with `csr = false`, so it ships no
  framework JavaScript.
- `src/app.html` holds the only script: the theme toggle. It applies the saved choice, otherwise the
  system appearance, to the page and its `theme-color` tag, and defaults to dark without
  JavaScript. Colours for both themes are CSS variables in `src/app.css`; the script repeats the
  two background colours for `theme-color`.
- `src/lib/site.ts` holds the version, architecture, minimum macOS, site URL and download URL,
  plus the name, title and description used by the page's metadata. Update it with each release.
  `static/robots.txt` and `static/sitemap.xml` repeat the site URL.
- `src/routes/download/+server.ts` answers `/download` with a 302 to
  `https://github.com/michaelshimeles/terminal-rs/releases/latest/download/terminal-rs-universal.dmg`.
  That URL works only once a GitHub release includes an asset named exactly
  `terminal-rs-universal.dmg`.

## Images

`assets-src/` holds the sources: two 3200x1920 window captures of the app (`hero-dark.png` and
`hero-light.png`), the icon (`icon.svg`) and the Open Graph card template (`og.html`).
`scripts/build-images.sh` regenerates everything in `static/` from them: AVIF and WebP screenshots
at 1080 and 2160 px wide, a cropped screenshot for phones, the icon, favicons, the touch icon and
`og.jpg`. It needs ImageMagick with AVIF support, `cwebp`, and Google Chrome or Chromium. The
script finds `google-chrome`, `google-chrome-stable`, `chromium` or `chromium-browser` on your
`PATH`, then Chrome in `/Applications`. To use another browser, set `CHROME` to its path or
command name, such as `CHROME=brave-browser`.

```sh
./scripts/build-images.sh
```

The icon is a placeholder. The app bundle doesn't ship an icon yet; replace `assets-src/icon.svg`
when it does.
