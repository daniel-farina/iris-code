// Tests for collision.js and player.js

import { test } from 'node:test';
import assert from 'node:assert/strict';

test('collision triggers when player overlaps cube at origin', async () => {
  const THREE = await import('three');
  const { createPlayer } = await import('../src/entities/player.js');
  const { checkCollisions } = await import('../src/systems/collision.js');

  const scene = { add() {} };
  const input = { keys: {} };
  const player = createPlayer(scene, input);

  // Create a cube entity at origin (same as player)
  const cubeMesh = new THREE.Mesh(new THREE.BoxGeometry(0.8, 0.8, 0.8));
  cubeMesh.position.set(0, 0, 0);
  const entity = { mesh: cubeMesh };

  let hit = false;
  checkCollisions(player, [entity], () => { hit = true; });
  assert.ok(hit, 'collision should have been detected');
});

test('no collision when player is far from cube', async () => {
  const THREE = await import('three');
  const { createPlayer } = await import('../src/entities/player.js');
  const { checkCollisions } = await import('../src/systems/collision.js');

  const scene = { add() {} };
  const input = { keys: {} };
  const player = createPlayer(scene, input);

  // Move player far away
  player.mesh.position.set(100, 0, 100);

  // Cube at origin
  const cubeMesh = new THREE.Mesh(new THREE.BoxGeometry(0.8, 0.8, 0.8));
  cubeMesh.position.set(0, 0, 0);
  const entity = { mesh: cubeMesh };

  let hit = false;
  checkCollisions(player, [entity], () => { hit = true; });
  assert.ok(!hit, 'collision should NOT have been detected');
});
