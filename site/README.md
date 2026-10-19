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

- `src/routes/+page.svelte` is the page. It is prerendered and then hydrated, because the demo and
  the feature cards animate in the browser. Without JavaScript the page still reads: the demo
  shows the app window and every section is visible.
- `src/lib/demo/` is the hero demo. `script.ts` holds the 20-second timeline: when each command is
  typed, the camera moves, the captions and the sound cues. `Scene.svelte` draws one frame for a
  given time over the app screenshot, `player.svelte.ts` advances the time, and `sound.ts`
  synthesizes the sounds with the Web Audio API, so there are no audio files. The demo plays muted
  on a loop while it is on screen; "Play with sound" restarts it once with sound.
- `src/lib/cards/` holds the feature card animations. Each one loops only while it is on screen.
- With reduced motion, the demo and cards hold one still frame and nothing fades in on scroll.
- The theme button in the nav switches the page, the demo and the cards between dark and light,
  and remembers the choice. Dark is the default. The page colors are in `src/app.css`, including
  the app's own Terminal Dark and Terminal Light colors under `.app`.
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
`scripts/build-images.sh` regenerates everything in `static/` from them: the two captures as
full-size WebP for the demo (it zooms into the chart, so they stay at 3200 px), the icon,
favicons, the touch icon and `og.jpg`. It needs ImageMagick, `cwebp`, and Google Chrome or
Chromium. The script finds `google-chrome`, `google-chrome-stable`, `chromium` or
`chromium-browser` on your `PATH`, then Chrome in `/Applications`. To use another browser, set
`CHROME` to its path or command name, such as `CHROME=brave-browser`.

```sh
./scripts/build-images.sh
```

The icon is the candlestick mark from `src/lib/Logo.svelte`. The app bundle doesn't ship an icon yet; if it
gets one, update both.
