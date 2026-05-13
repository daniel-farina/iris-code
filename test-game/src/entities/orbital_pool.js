// Shared factory for orbiting entities (cubes or spheres).
// Returns { items, update } — same shape as the old createOrbiters/createSpheres.

import * as THREE from 'three';

export function createOrbitalPool(scene, opts = {}) {
  const { count = 8, geometry = 'box', speedSign = 1 } = opts;
  const items = [];
  for (let i = 0; i < count; i++) {
    const hue = i / count;
    const mat = new THREE.MeshStandardMaterial({
      color: new THREE.Color().setHSL(hue, 0.7, 0.55),
    });
    const geo = geometry === 'sphere'
      ? new THREE.SphereGeometry(0.5, 16, 16)
      : new THREE.BoxGeometry(0.8, 0.8, 0.8);
    const mesh = new THREE.Mesh(geo, mat);
    items.push({
      mesh,
      radius: 3 + i * 0.4,
      speed: speedSign * (0.4 + i * 0.05),
      phase: (i / count) * Math.PI * 2,
      bobAmp: 0.3 + (i % 3) * 0.2,
    });
    scene.add(mesh);
  }
  function update(_dt, t) {
    for (const it of items) {
      const a = it.phase + t * it.speed;
      it.mesh.position.x = Math.cos(a) * it.radius;
      it.mesh.position.z = Math.sin(a) * it.radius;
      it.mesh.position.y = Math.sin(t * 2 + it.phase) * it.bobAmp;
      it.mesh.rotation.x += 0.02;
      it.mesh.rotation.y += 0.03;
    }
  }
  return { items, update };
}
