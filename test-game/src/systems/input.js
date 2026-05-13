// Keyboard input system. Tracks pressed keys plus previous-frame state
// so callers can do edge-detect ('just-pressed') via `keys.X && !prev.X`.

export function createInput() {
  const keys = {};
  const prev = {};
  addEventListener('keydown', (e) => {
    keys[e.code] = true;
  });
  addEventListener('keyup', (e) => {
    keys[e.code] = false;
  });
  return {
    keys,
    get spacePrev() {
      return prev.Space ?? false;
    },
    frameEnd() {
      // Snapshot this frame's state into prev for next-frame edge detect.
      for (const k of Object.keys(keys)) prev[k] = keys[k];
    },
  };
}
