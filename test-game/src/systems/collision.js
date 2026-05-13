import * as THREE from 'three';

const _box = new THREE.Box3();

export function checkCollisions(player, entitiesArray, onHit) {
  const playerBounds = player.getBounds();
  for (const entity of entitiesArray) {
    _box.setFromObject(entity.mesh);
    if (playerBounds.intersectsBox(_box)) {
      onHit(entity);
    }
  }
}
