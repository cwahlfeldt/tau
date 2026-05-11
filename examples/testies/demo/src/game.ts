import * as THREE from 'three';
import { createWorld, addEntity, addComponent, query } from 'bitecs';

// Components — plain objects, parallel arrays indexed by entity id.
const Rotation = { x: [] as number[], y: [] as number[] };

const world = createWorld();
const cube = addEntity(world);
addComponent(world, cube, Rotation);
Rotation.x[cube] = 0;
Rotation.y[cube] = 0;

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(devicePixelRatio);
renderer.setSize(innerWidth, innerHeight);
document.body.appendChild(renderer.domElement);

const scene = new THREE.Scene();
const camera = new THREE.PerspectiveCamera(70, innerWidth / innerHeight, 0.1, 1000);
camera.position.z = 3;

const mesh = new THREE.Mesh(new THREE.BoxGeometry(), new THREE.MeshNormalMaterial());
scene.add(mesh);

addEventListener('resize', () => {
  camera.aspect = innerWidth / innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight);
});

const hud = document.getElementById('hud')!;
let frames = 0;
let fpsAcc = 0;
let lastFpsUpdate = performance.now();

let last = performance.now();
renderer.setAnimationLoop(() => {
  const now = performance.now();
  const dt = (now - last) / 1000;
  last = now;

  for (const entity of query(world, [Rotation])) {
    Rotation.x[entity] += 0.6 * dt;
    Rotation.y[entity] += 0.6 * dt;
  }

  mesh.rotation.x = Rotation.x[cube];
  mesh.rotation.y = Rotation.y[cube];
  renderer.render(scene, camera);

  frames++;
  fpsAcc += dt;
  if (now - lastFpsUpdate >= 250) {
    hud.textContent = `${(frames / fpsAcc).toFixed(0)} fps`;
    frames = 0;
    fpsAcc = 0;
    lastFpsUpdate = now;
  }
});
