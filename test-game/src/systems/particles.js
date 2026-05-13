import * as THREE from 'three';

const POOL_SIZE = 200;
const BASE_SCALE = 0.08;

function createPool(scene) {
  const pool = [];
  const geo = new THREE.SphereGeometry(1, 4, 4);
  for (let i = 0; i < POOL_SIZE; i++) {
    const mat = new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0 });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.visible = false;
    mesh.scale.setScalar(0);
    scene.add(mesh);
    pool.push({ mesh, vx: 0, vy: 0, vz: 0, life: 0, maxLife: 1, active: false });
  }
  return pool;
}

export function createParticles(scene) {
  const pool = createPool(scene);
  let activeCount = 0;

  function spawnBurst(pos, color, count = 20) {
    let spawned = 0;
    const col = new THREE.Color(color);
    for (let i = 0; i < pool.length && spawned < count; i++) {
      const p = pool[i];
      if (p.active) continue;
      p.active = true;
      p.life = 1;
      p.maxLife = 0.6 + Math.random() * 0.8;
      p.mesh.position.copy(pos);
      p.mesh.material.color.copy(col);
      p.mesh.material.opacity = 1;
      p.mesh.visible = true;
      p.mesh.scale.setScalar(BASE_SCALE);
      // random outward velocity
      const theta = Math.random() * Math.PI * 2;
      const phi = Math.acos(2 * Math.random() - 1);
      const speed = 1.5 + Math.random() * 2.5;
      p.vx = Math.sin(phi) * Math.cos(theta) * speed;
      p.vy = Math.sin(phi) * Math.sin(theta) * speed;
      p.vz = Math.cos(phi) * speed;
      spawned++;
      activeCount++;
    }
    return spawned;
  }

  function update(dt) {
    for (const p of pool) {
      if (!p.active) continue;
      p.life -= dt / p.maxLife;
      if (p.life <= 0) {
        p.active = false;
        p.mesh.visible = false;
        activeCount--;
        continue;
      }
      p.mesh.position.x += p.vx * dt;
      p.mesh.position.y += p.vy * dt;
      p.mesh.position.z += p.vz * dt;
      p.mesh.material.opacity = p.life;
      p.mesh.scale.setScalar(BASE_SCALE * p.life);
    }
  }

  return { spawnBurst, update, pool, get activeCount() { return activeCount; } };
}
