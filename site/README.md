# The Anser website

One static page, plain HTML and CSS: no build step, no JavaScript, no external
requests. It is published at <https://lucianosrp.github.io/anser/>.

- `index.html`, `styles.css`, `favicon.svg`: the page.
- `img/`: crops of the demo-household screenshots in `desktop/docs/screenshots`
  (made-up data only, never real data).
- `fonts/`: Noto Sans, Noto Sans Mono and a Noto Serif Italic subset, copied
  from `desktop/web/fonts` (SIL Open Font License, `fonts/OFL.txt`).
- `og.html` and `og.png`: the 1200x630 social card and its source.

## Preview

```bash
python3 -m http.server -d site     # from the repo root, then open http://localhost:8000
```

## The social card

After changing `og.html`, render it again with headless Chrome at 1x:

```bash
google-chrome-stable --headless=new --disable-gpu --hide-scrollbars \
  --window-size=1200,630 --screenshot=site/og.png "file://$PWD/site/og.html"
```

## Deploy

`.github/workflows/pages.yml` uploads this folder as-is to GitHub Pages on
every push to `main` that touches `site/**` (or the workflow), and on demand
from the Actions tab. The repository's Pages source must be set to
**GitHub Actions** (Settings, Pages).
