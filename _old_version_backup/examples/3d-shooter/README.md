# 3d-shooter

GTA-style 3D sandbox + wave-shooter, built with three.js + Vite. Driven entirely by
[hip](../../README.md) and the autonomous `/loop` skill from a punch-list at
`/Users/dan/.hip-loop/3d-shooter` (no manual edits to the game source for the
bulk of development).

## Live demo

https://hippo-code.com/demo/

The built bundle is committed to [`docs/demo/`](../../docs/demo/) so GitHub Pages
serves it from the same site that hosts the project landing page.

## Features

- Procedural city: ~60 buildings, traffic lights, ambient cars, NPCs, police
- Day/night cycle, weather, shader sky, exp fog
- 5 camera modes: chase, FPV, top-down, orbit, cinematic demo tour
- 32 enterable doors -> 17 procedural room themes (office, bar, music studio,
  greenhouse, gym, library, cinema, arcade, garage, ...)
- 30+ ambient scene elements: airport, cruise ship, ferris wheel, stadium,
  lighthouse, UFO, drive-in, monument, helicopter, crane, ...

## Controls

WASD move - Mouse look - Click shoot - Space jump - Shift sprint - C crouch - E
enter vehicle/door - R reload - 1/2/3/4/5 camera mode - T cycle - H help

## Where the source lives

The full Vite project (with HMR) is at `/Users/dan/.hip-loop/3d-shooter` on the
maintainer's machine. Only the built bundle ships here.

## Rebuilding / republishing

See [DEPLOY_PAGE.md](../../DEPLOY_PAGE.md) at the repo root.
