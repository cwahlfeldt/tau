import React, { useEffect, useMemo, useRef, useState } from "react";
import * as THREE from "three";
import { Canvas, useFrame } from "@react-three/fiber";
import { OrbitControls } from "@react-three/drei";

// Tauri plugins are exposed by tau at window.__TAURI_PLUGIN_*. No npm
// imports needed — withGlobalTauri injects these as a webview init script.
const fs = () => window.__TAURI_PLUGIN_FS__;
const dialog = () => window.__TAURI_PLUGIN_DIALOG__;
const notification = () => window.__TAURI_PLUGIN_NOTIFICATION__;

const COUNTER_FILE = "r3f-demo-counter.txt";

// Tauri v2 BaseDirectory enum — see tauri::path::BaseDirectory in the
// Rust source. AppData = 14 maps to the per-app data directory, which is
// the only fs scope granted by tau's default capability.
const BASE_APP_DATA = 14;

function Cube({ color, position, onClick, label }) {
  const ref = useRef();
  const [hovered, setHovered] = useState(false);
  useFrame((_, dt) => {
    ref.current.rotation.x += dt * 0.4;
    ref.current.rotation.y += dt * 0.6;
  });
  return (
    <group position={position}>
      <mesh
        ref={ref}
        onClick={onClick}
        onPointerOver={() => setHovered(true)}
        onPointerOut={() => setHovered(false)}
        scale={hovered ? 1.15 : 1}
      >
        <boxGeometry args={[1, 1, 1]} />
        <meshStandardMaterial color={hovered ? "#ffffff" : color} />
      </mesh>
      <mesh position={[0, -0.9, 0]}>
        <planeGeometry args={[1.6, 0.35]} />
        <meshBasicMaterial color="#000" transparent opacity={0.35} />
      </mesh>
      <Label position={[0, -0.9, 0.01]}>{label}</Label>
    </group>
  );
}

function Label({ position, children }) {
  // drei's <Text> pulls in font assets we don't want to ship; a flat HTML
  // overlay would be the simpler alternative, but the cubes orbit, so we
  // keep labels in-scene as plain meshes with a CanvasTexture.
  const texture = useMemo(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 512;
    canvas.height = 128;
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#ffffff";
    ctx.font = "bold 64px system-ui, sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(children, canvas.width / 2, canvas.height / 2);
    return new THREE.CanvasTexture(canvas);
  }, [children]);
  return (
    <mesh position={position}>
      <planeGeometry args={[1.6, 0.4]} />
      <meshBasicMaterial map={texture} transparent />
    </mesh>
  );
}

export default function App() {
  const [status, setStatus] = useState({ msg: "ready — click a cube", kind: "" });
  const [counter, setCounter] = useState(null);
  const [tauriReady, setTauriReady] = useState(false);

  useEffect(() => {
    const ready = !!(fs() && dialog() && notification());
    setTauriReady(ready);
    if (!ready) {
      setStatus({
        msg: "Tauri plugins not available — open this in `tau` to enable",
        kind: "err",
      });
      return;
    }
    readCounter().then((n) => setCounter(n));
  }, []);

  const log = (msg, kind = "ok") => setStatus({ msg, kind });

  async function readCounter() {
    try {
      const exists = await fs()
        .exists(COUNTER_FILE, { baseDir: BASE_APP_DATA })
        .catch(() => false);
      if (!exists) return 0;
      const txt = await fs().readTextFile(COUNTER_FILE, { baseDir: BASE_APP_DATA });
      return Number(txt) || 0;
    } catch {
      return 0;
    }
  }

  async function bumpCounter() {
    if (!tauriReady) return;
    try {
      // Tauri's writeTextFile won't auto-create the parent dir on first run.
      // mkdir with recursive:true is a no-op if the dir already exists.
      await fs()
        .mkdir("", { baseDir: BASE_APP_DATA, recursive: true })
        .catch(() => {});
      const next = (counter ?? 0) + 1;
      await fs().writeTextFile(COUNTER_FILE, String(next), { baseDir: BASE_APP_DATA });
      setCounter(next);
      log(`fs: wrote counter=${next} to appdata`);
    } catch (e) {
      log(`fs error: ${e?.message ?? e}`, "err");
    }
  }

  async function pickFile() {
    if (!tauriReady) return;
    try {
      const path = await dialog().open({ multiple: false });
      if (path == null) {
        log("dialog: cancelled");
      } else {
        log(`dialog: ${path}`);
      }
    } catch (e) {
      log(`dialog error: ${e?.message ?? e}`, "err");
    }
  }

  async function notify() {
    if (!tauriReady) return;
    try {
      let granted = await notification().isPermissionGranted();
      if (!granted) {
        const res = await notification().requestPermission();
        granted = res === "granted";
      }
      if (!granted) {
        log("notification: permission denied", "err");
        return;
      }
      await notification().sendNotification({
        title: "tau r3f-demo",
        body: "Hello from a Tauri-wrapped React Three Fiber app.",
      });
      log("notification: sent");
    } catch (e) {
      log(`notification error: ${e?.message ?? e}`, "err");
    }
  }

  return (
    <>
      <div className="hud">
        <h1>tau · r3f-demo</h1>
        <p>Click a cube to call a Tauri plugin. Drag to orbit.</p>
        <p>fs counter on disk: <strong>{counter ?? "—"}</strong></p>
      </div>

      <Canvas camera={{ position: [0, 1.5, 5], fov: 50 }}>
        <ambientLight intensity={0.5} />
        <directionalLight position={[5, 5, 5]} intensity={0.8} />
        <Cube
          color="#5a7dff"
          position={[-2.2, 0, 0]}
          label="notify"
          onClick={notify}
        />
        <Cube
          color="#ff9f43"
          position={[0, 0, 0]}
          label="open file"
          onClick={pickFile}
        />
        <Cube
          color="#8fe388"
          position={[2.2, 0, 0]}
          label="fs counter+"
          onClick={bumpCounter}
        />
        <OrbitControls enablePan={false} />
      </Canvas>

      <div className={`status ${status.kind}`}>{status.msg}</div>
    </>
  );
}
