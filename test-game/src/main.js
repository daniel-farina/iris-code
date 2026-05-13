// hip-ts test game: minimal three.js orbiting cubes. Modular by design so
// the model has clear seams to extend (add scene/, entities/, systems/).
//
// This is the SANDBOX hip-ts will be asked to extend over iters. Keep the
// initial surface deliberately small and well-named so test prompts can
// target specific files.

import * as THREE from 'three';
import { createOrbitalPool } from './entities/orbital_pool.js';
import { createPlayer } from './entities/player.js';
import { createInput } from './systems/input.js';
import { createParticles } from './systems/particles.js';
import { createTrail } from './systems/trail.js';
import { checkCollisions } from './systems/collision.js';

const scene = new THREE.Scene();
scene.background = new THREE.Color(0x101820);

const camera = new THREE.PerspectiveCamera(70, innerWidth / innerHeight, 0.1, 200);
camera.position.set(0, 4, 12);
camera.lookAt(0, 0, 0);

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setSize(innerWidth, innerHeight);
renderer.setPixelRatio(Math.min(devicePixelRatio, 1.5));
document.body.appendChild(renderer.domElement);

scene.add(new THREE.AmbientLight(0xffffff, 0.5));
const sun = new THREE.DirectionalLight(0xffffff, 1.0);
sun.position.set(5, 10, 7);
scene.add(sun);

const ground = new THREE.Mesh(
  new THREE.PlaneGeometry(40, 40),
  new THREE.MeshStandardMaterial({ color: 0x2a2a2a, roughness: 0.95 }),
);
ground.rotation.x = -Math.PI / 2;
ground.position.y = -1;
scene.add(ground);

const cubes = createOrbitalPool(scene, { count: 8, geometry: 'box', speedSign: 1 });
const orbs = createOrbitalPool(scene, { count: 6, geometry: 'sphere', speedSign: -1 });
const input = createInput();
const particles = createParticles(scene);
const player = createPlayer(scene, input);
const trail = createTrail(scene, { color: 0x00ffff });
const scoreEl = document.getElementById('score');
let score = 0;

const clock = new THREE.Clock();
function loop() {
  const dt = Math.min(clock.getDelta(), 0.05);
  const t = clock.getElapsedTime();
  cubes.update(dt, t);
  orbs.update(dt, t);
  player.update(dt);
  trail.update(dt, player.mesh);
  checkCollisions(player, cubes.items, (entity) => {
    score++;
    scoreEl.textContent = String(score);
    particles.spawnBurst(entity.mesh.position, 0xffff00);
  });
  particles.update(dt);
  if (input.keys.Space && !input.spacePrev) {
    score++;
    scoreEl.textContent = String(score);
  }
  input.frameEnd();
  renderer.render(scene, camera);
  requestAnimationFrame(loop);
}
loop();

addEventListener('resize', () => {
  camera.aspect = innerWidth / innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight);
});
