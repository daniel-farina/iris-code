import * as THREE from 'three';

export function createPlayer(scene, input) {
  const size = 0.6;
  const mesh = new THREE.Mesh(
    new THREE.BoxGeometry(size, size, size),
    new THREE.MeshStandardMaterial({ color: 0xff0000 }),
  );
  mesh.position.set(0, 0, 0);
  scene.add(mesh);

  const speed = 6;
  const bounds = new THREE.Box3();

  function update(dt) {
    let dx = 0, dz = 0;
    if (input.keys.KeyW) dz -= 1;
    if (input.keys.KeyS) dz += 1;
    if (input.keys.KeyA) dx -= 1;
    if (input.keys.KeyD) dx += 1;
    if (dx !== 0 || dz !== 0) {
      const len = Math.sqrt(dx * dx + dz * dz);
      dx /= len;
      dz /= len;
      const sprint = input.keys.ShiftLeft ? 1.8 : 1;
      mesh.position.x += dx * speed * sprint * dt;
      mesh.position.z += dz * speed * sprint * dt;
    }
  }

  function getBounds() {
    bounds.setFromObject(mesh);
    return bounds;
  }

  return { mesh, update, getBounds };
}
