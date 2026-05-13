import { test } from 'node:test';
import assert from 'node:assert/strict';

const { createParticles } = await import('../src/systems/particles.js');

function makeScene() {
  return { added: [], add(o) { this.added.push(o); } };
}

test('spawnBurst increases active count', async () => {
  const p = createParticles(makeScene());
  assert.equal(p.activeCount, 0);
  const spawned = p.spawnBurst({ x: 0, y: 0, z: 0 }, 0xff0000, 10);
  assert.equal(spawned, 10);
  assert.equal(p.activeCount, 10);
});

test('update advances positions', async () => {
  const scene = makeScene();
  const p = createParticles(scene);
  p.spawnBurst({ x: 0, y: 0, z: 0 }, 0x00ff00, 5);
  // pool is exposed for testing
  const activeMeshes = p.pool.filter(e => e.active).map(e => e.mesh);
  const before = activeMeshes.map(m => ({ x: m.position.x, y: m.position.y, z: m.position.z }));
  p.update(0.05);
  const after = activeMeshes.map(m => ({ x: m.position.x, y: m.position.y, z: m.position.z }));
  let moved = false;
  for (let i = 0; i < before.length; i++) {
    if (before[i].x !== after[i].x || before[i].y !== after[i].y || before[i].z !== after[i].z) {
      moved = true;
      break;
    }
  }
  assert.ok(moved, 'at least one particle position should change after update');
});

test('pool caps at 200', async () => {
  const p = createParticles(makeScene());
  const spawned = p.spawnBurst({ x: 0, y: 0, z: 0 }, 0x0000ff, 300);
  assert.ok(spawned <= 200, `spawned ${spawned}, expected <= 200`);
  assert.equal(spawned, 200, 'should fill entire pool');
  assert.equal(p.activeCount, 200);
});
