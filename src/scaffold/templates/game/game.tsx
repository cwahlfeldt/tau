import { createRoot } from 'react-dom/client';
import { useRef, useState, useEffect } from 'react';
import { Canvas, useFrame } from 'tau';
import * as THREE from 'three';

function RotatingBox() {
  const ref = useRef<THREE.Mesh>(null);
  useFrame((_, dt) => {
    if (!ref.current) return;
    ref.current.rotation.x += 0.6 * dt;
    ref.current.rotation.y += 0.6 * dt;
  });
  return (
    <mesh ref={ref}>
      <boxGeometry />
      <meshNormalMaterial />
    </mesh>
  );
}

// HUD lives outside the <Canvas> tree, so useFrame isn't available here —
// drive its own rAF instead. r3f's useFrame only fires for components
// rendered inside a Canvas.
function Hud() {
  const [fps, setFps] = useState(0);
  useEffect(() => {
    let frames = 0;
    let acc = 0;
    let last = performance.now();
    let raf = 0;
    const tick = (now: number) => {
      const dt = (now - last) / 1000;
      last = now;
      frames++;
      acc += dt;
      if (acc >= 0.25) {
        setFps(Math.round(frames / acc));
        frames = 0;
        acc = 0;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, []);
  return <div id="hud">{fps} fps</div>;
}

function App() {
  return (
    <>
      <Canvas camera={{ position: [0, 0, 3], fov: 70 }}>
        <RotatingBox />
      </Canvas>
      <Hud />
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
