import { useEffect, useRef } from 'react';
import ReactDOM from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import {
  JarvisOrb,
  type JarvisPaletteValues,
  type JarvisStateTarget,
} from 'jarvis-ai-web-animation';
import './splashscreen.css';

const E_ASSISANT_PALETTE: JarvisPaletteValues = {
  core: 0xffdf78,
  primary: 0x8d181d,
  secondary: 0xd29a2e,
  tertiary: 0x4d0d12,
  deep: 0x170507,
  fallback: 'radial-gradient(circle at 42% 38%, #fff1a6 0%, #d29a2e 18%, #8d181d 42%, #4d0d12 66%, #170507 82%, transparent 100%)',
};

const STARTUP_STATE: JarvisStateTarget = {
  energy: 1.35,
  rotationSpeed: 0.62,
  particleSpeed: 1.45,
  shellRadius: 1.05,
  ringSpread: 1.08,
  filamentOpacity: 0.78,
  coreScale: 1.04,
  bloom: 0.92,
};

const PARTICLE_COLORS = ['#f5c451', '#d29a2e', '#9d2025', '#ffd978', '#5f1015'];

type StartupParticle = {
  angle: number;
  distance: number;
  depth: number;
  drift: number;
  size: number;
  color: string;
  phase: number;
};

function easeOutCubic(value: number) {
  return 1 - Math.pow(1 - value, 3);
}

function StartupField() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const context = canvas.getContext('2d', { alpha: false });
    if (!context) return;

    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const particles: StartupParticle[] = [];
    const startedAt = performance.now();
    let width = 0;
    let height = 0;
    let ratio = 1;
    let frame = 0;

    const resetParticles = () => {
      particles.length = 0;
      const count = Math.min(250, Math.max(130, Math.round((width * height) / 7800)));
      const reach = Math.hypot(width, height) * 0.62;
      for (let index = 0; index < count; index += 1) {
        const angle = Math.random() * Math.PI * 2;
        particles.push({
          angle,
          distance: 58 + Math.pow(Math.random(), 0.48) * reach,
          depth: 0.28 + Math.random() * 0.72,
          drift: (Math.random() - 0.5) * 0.75,
          size: 0.4 + Math.random() * 1.25,
          color: PARTICLE_COLORS[Math.floor(Math.random() * PARTICLE_COLORS.length)],
          phase: Math.random() * Math.PI * 2,
        });
      }
    };

    const resize = () => {
      ratio = Math.min(window.devicePixelRatio || 1, 1.5);
      width = window.innerWidth;
      height = window.innerHeight;
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      resetParticles();
    };

    const draw = (now: number) => {
      const elapsed = now - startedAt;
      const centerX = width / 2;
      const centerY = height * 0.47;
      const gather = Math.min(1, elapsed / 760);
      const burst = Math.max(0, Math.min(1, (elapsed - 1300) / 520));
      const gatherEase = easeOutCubic(gather);
      const burstEase = burst * burst;

      context.fillStyle = '#01030a';
      context.fillRect(0, 0, width, height);
      context.save();
      context.globalCompositeOperation = 'lighter';

      for (const particle of particles) {
        const rotation = particle.angle
          + elapsed * 0.00018 * particle.depth
          + particle.drift * (1 - gatherEase);
        const gatheredDistance = particle.distance * (1 - gatherEase * 0.9);
        const currentDistance = gatheredDistance + burstEase * particle.distance * 2.8;
        const wave = Math.sin(elapsed * 0.003 + particle.phase) * 5 * (1 - burst);
        const x = centerX + Math.cos(rotation) * (currentDistance + wave);
        const y = centerY + Math.sin(rotation) * (currentDistance * 0.58 + wave);
        const opacity = burst > 0 ? (1 - burst) * 0.8 : 0.12 + gatherEase * 0.62;
        const size = particle.size * (0.65 + particle.depth) * (1 + burstEase * 1.8);

        context.globalAlpha = Math.max(0, opacity);
        context.strokeStyle = particle.color;
        context.fillStyle = particle.color;
        if (burst > 0.04) {
          const tail = 16 + burstEase * 76 * particle.depth;
          context.lineWidth = Math.max(0.35, size * 0.55);
          context.beginPath();
          context.moveTo(x, y);
          context.lineTo(
            x - Math.cos(rotation) * tail,
            y - Math.sin(rotation) * tail * 0.58,
          );
          context.stroke();
        } else {
          context.beginPath();
          context.arc(x, y, size, 0, Math.PI * 2);
          context.fill();
        }
      }
      context.restore();

      if (elapsed < 2050 && !reducedMotion) {
        frame = requestAnimationFrame(draw);
      }
    };

    resize();
    window.addEventListener('resize', resize);
    frame = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('resize', resize);
    };
  }, []);

  return <canvas ref={canvasRef} id="startup-field" aria-hidden="true" />;
}

function Splashscreen() {
  useEffect(() => {
    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const timer = window.setTimeout(
      () => invoke('mark_splash_animation_complete').catch((error) => {
        console.error('Failed to finish splash animation', error);
      }),
      reducedMotion ? 350 : 1980,
    );
    return () => window.clearTimeout(timer);
  }, []);

  return (
    <div className="splash-stage">
      <StartupField />
      <main className="startup-mark" aria-label="E-Assisant 正在启动">
        <div className="jarvis-orb-stage">
          <JarvisOrb
            size="hero"
            state={STARTUP_STATE}
            palette={E_ASSISANT_PALETTE}
            quality="balanced"
            intensity={1.12}
            interactive={false}
            breathing
            breathingIntensity={0.72}
            ariaLabel="E-Assisant 智能核心"
          />
        </div>
        <div className="startup-copy">
          <div className="eyebrow">LOCAL INTELLIGENCE SYSTEM</div>
          <h1>E-ASSISANT</h1>
          <div className="status"><span />SYSTEM ONLINE</div>
        </div>
      </main>
      <div className="scanline" aria-hidden="true" />
    </div>
  );
}

ReactDOM.createRoot(document.getElementById('splash-root')!).render(<Splashscreen />);
