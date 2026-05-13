import * as THREE from 'three';

const POOL_SIZE = 50;
const MARKER_RADIUS = 0.1;
const FADE_TIME = 0.8;
const SPAWN_DISTANCE = 0.3;

export function createTrail(scene, opts = {}) {
  const color = opts.color ?? 0xffffff;
  const pool = [];
  const geo = new THREE.SphereGeometry(MARKER_RADIUS, 4, 4);

  for (let i = 0; i < POOL_SIZE; i++) {
    const mat = new THREE.MeshBasicMaterial({ color, transparent: true, opacity: 0 });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.visible = false;
    scene.add(mesh);
    pool.push({ mesh, life: 0, active: false });
  }

  let prevPos = null;

  function update(dt, target) {
    const pos = target.position;
    const px = pos.x, py = pos.y, pz = pos.z;

    if (prevPos) {
      const dx = px - prevPos.x, dy = py - prevPos.y, dz = pz - prevPos.z;
      if (Math.sqrt(dx * dx + dy * dy + dz * dz) > SPAWN_DISTANCE) {
        for (const p of pool) {
          if (!p.active) {
            p.active = true;
            p.life = FADE_TIME;
            p.mesh.position.set(px, py, pz);
            p.mesh.material.opacity = 1;
            p.mesh.visible = true;
            break;
          }
        }
        prevPos = { x: px, y: py, z: pz };
      }
    } else {
      prevPos = { x: px, y: py, z: pz };
    }

    for (const p of pool) {
      if (!p.active) continue;
      p.life -= dt;
      if (p.life <= 0) {
        p.active = false;
        p.mesh.visible = false;
        continue;
      }
      p.mesh.material.opacity = p.life / FADE_TIME;
    }
  }

  return { update, pool };
}
