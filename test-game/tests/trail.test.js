import { test } from 'node:test';
import assert from 'node:assert/strict';

const { createTrail } = await import('../src/systems/trail.js');

function makeScene() {
  return { added: [], add(o) { this.added.push(o); } };
}

function makeTarget(x, y, z) {
  return { position: { x: x ?? 0, y: y ?? 0, z: z ?? 0 } };
}

test('update spawns markers when target moves', async () => {
  const scene = makeScene();
  const trail = createTrail(scene, { color: 0x00ffff });
  const target = makeTarget(0, 0, 0);

  // First update — no movement yet, should not spawn
  trail.update(0.016, target);
  const activeAfterFirst = trail.pool.filter(p => p.active).length;
  assert.equal(activeAfterFirst, 0, 'no markers spawned on first stationary update');

  // Move target far enough to trigger spawn
  target.position.x = 1;
  target.position.y = 0;
  target.position.z = 0;
  trail.update(0.016, target);

  const activeAfterMove = trail.pool.filter(p => p.active).length;
  assert.ok(activeAfterMove > 0, 'should spawn at least one marker when target moves');
});

test('markers expire after 1s', async () => {
  const scene = makeScene();
  const trail = createTrail(scene, { color: 0x00ffff });
  const target = makeTarget(0, 0, 0);

  // First update establishes prevPos at origin
  trail.update(0.016, target);

  // Move target to spawn a marker
  target.position.x = 1;
  trail.update(0.016, target);

  let active = trail.pool.filter(p => p.active).length;
  assert.ok(active > 0, 'marker should be active after spawn');

  // Advance time past fade time (0.8s) + buffer
  trail.update(1.0, target);

  active = trail.pool.filter(p => p.active).length;
  assert.equal(active, 0, 'all markers should have expired after 1s');
});
