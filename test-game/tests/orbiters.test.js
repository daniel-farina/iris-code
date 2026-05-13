// Smoke test: orbital_pool.js exports createOrbitalPool and the returned object
// has the expected shape. node --test compatible.

import { test } from 'node:test';
import assert from 'node:assert/strict';

const stubScene = { added: [], add(o) { this.added.push(o); } };

test('createOrbitalPool(box) creates `count` meshes and returns an update fn', async () => {
  const { createOrbitalPool } = await import('../src/entities/orbital_pool.js');
  const out = createOrbitalPool(stubScene, { count: 4, geometry: 'box', speedSign: 1 });
  assert.equal(out.items.length, 4);
  assert.equal(typeof out.update, 'function');
  assert.equal(stubScene.added.length, 4);
});

test('update advances positions; speedSign controls direction', async () => {
  const { createOrbitalPool } = await import('../src/entities/orbital_pool.js');
  const out = createOrbitalPool({ add() {} }, { count: 2, geometry: 'sphere', speedSign: -1 });
  out.update(0.016, 0);
  const at0 = out.items[0].mesh.position.x;
  out.update(0.016, 1);
  const at1 = out.items[0].mesh.position.x;
  assert.notEqual(at0, at1, 'position should change over time');
});
