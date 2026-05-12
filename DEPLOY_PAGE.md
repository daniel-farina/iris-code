# Deploy notes

GitHub Pages serves this repo from the `docs/` folder on the `main` branch.
Custom domain: **hippo-code.com** (configured via `docs/CNAME`).

## Routes

| Path                                     | What it is                                    |
|------------------------------------------|-----------------------------------------------|
| https://hippo-code.com/                  | Project landing page (`docs/index.html`)      |
| https://hippo-code.com/demo/             | 3d-shooter game (`docs/demo/`)                |

## Republishing the 3d-shooter game (`/demo/`)

The game source lives outside this repo at `/Users/dan/.hip-loop/3d-shooter`
(Vite + three.js). Only the built bundle is committed under `docs/demo/`.

To rebuild and republish:

```bash
# 1. Build the game with the right base path so asset URLs resolve under /demo/
cd /Users/dan/.hip-loop/3d-shooter
npm run build -- --base=/demo/

# 2. Drop the built files into this repo's docs/demo/
cd /Users/dan/code-2/mlx-code
rm -rf docs/demo
mkdir -p docs/demo
cp -R /Users/dan/.hip-loop/3d-shooter/dist/* docs/demo/

# 3. Commit + push
git add docs/demo examples/3d-shooter
git commit -m "demo: republish 3d-shooter"
git push origin main
```

GitHub Pages picks up the change within ~1 minute.

## Adding a new sub-page (e.g. `/something/`)

1. Build the page with the right base path: `--base=/something/` (or absolute
   paths everywhere, but base is easier with Vite).
2. Copy the built artifacts to `docs/something/`.
3. Commit `docs/something/` on `main`. Pages will serve it at
   `https://hippo-code.com/something/` automatically.

## Why `docs/` and not `gh-pages`?

The repo is configured under **Settings -> Pages -> Build and deployment ->
Branch: `main` / `/docs`**. This means whatever lands in `docs/` on `main` ships
- no separate branch, no Pages workflow file. The trade-off is that the source
artifacts are visible in the main tree, but for this repo that's fine.

## CNAME

`docs/CNAME` contains the literal string `hippo-code.com`. Don't delete it; the
custom domain breaks if Pages re-deploys without that file.
