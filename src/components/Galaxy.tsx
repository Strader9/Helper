import { useEffect, useRef, type HTMLAttributes } from 'react';

export type GalaxyDirection = 'forward' | 'backward';

export interface GalaxyProps extends HTMLAttributes<HTMLDivElement> {
  starSpeed?: number;
  density?: number;
  disableAnimation?: boolean;
  speed?: number;
  mouseInteraction?: boolean;
  glowIntensity?: number;
  twinkleIntensity?: number;
  rotationSpeed?: number;
  starColors?: string[];
  starSize?: number;
  layers?: number;
  direction?: GalaxyDirection;
  backgroundColor?: string;
  opacity?: number;
  dpr?: number;
  focal?: [number, number];
  rotation?: [number, number];
  hueShift?: number;
  saturation?: number;
  mouseRepulsion?: boolean;
  repulsionStrength?: number;
  autoCenterRepulsion?: number;
  transparent?: boolean;
  lightMode?: boolean;
  flareIntensity?: number;
  clickWarp?: boolean;
  intro?: boolean;
}

type Star = { x: number; y: number; size: number; alpha: number; flare: number; color: string };
type Particle = { radius: number; angle: number; spread: number; size: number; alpha: number; color: string };
type Field = { stars: Star[]; arms: Particle[]; dust: Particle[] };

const COLORS = ['#f7fbff', '#a7dcff', '#8d9cff', '#b69cff'];
const TARGET_FRAME_MS = 1000 / 30;

function seededRandom(seed: number) {
  let value = seed >>> 0;
  return () => {
    value = (value * 1664525 + 1013904223) >>> 0;
    return value / 4294967296;
  };
}

function gaussian(random: () => number) {
  const u = Math.max(random(), 0.000001);
  const v = Math.max(random(), 0.000001);
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(Math.PI * 2 * v);
}

function createField(width: number, height: number, density: number, colors: string[], armTotal: number): Field {
  const random = seededRandom(Math.round(width * 17 + height * 31 + density * 997));
  const areaScale = Math.min(1.25, Math.max(0.72, (width * height) / (1440 * 900)));
  const starCount = Math.round(360 * density * areaScale);
  const armCount = Math.round(1500 * density * areaScale);
  const dustCount = Math.round(210 * density * areaScale);
  const stars = Array.from({ length: starCount }, (): Star => ({
    x: random(),
    y: random(),
    size: 0.42 + Math.pow(random(), 4.5) * 2.25,
    alpha: 0.24 + random() * 0.68,
    flare: Math.pow(random(), 8),
    color: colors[Math.floor(random() * colors.length)] ?? COLORS[0],
  }));

  const createParticle = (dust: boolean): Particle => {
    const radius = Math.pow(random(), dust ? 0.78 : 0.62);
    const arm = Math.floor(random() * armTotal);
    return {
      radius,
      angle: (arm / armTotal) * Math.PI * 2 + radius * Math.PI * 3.65 + gaussian(random) * (0.045 + radius * 0.18),
      spread: gaussian(random) * (dust ? 0.11 : 0.065) * (0.25 + radius),
      size: dust ? 5 + random() * 12 : 0.38 + Math.pow(random(), 3) * 1.9,
      alpha: dust ? 0.016 + random() * 0.034 : 0.17 + random() * 0.76,
      color: colors[Math.floor(random() * colors.length)] ?? COLORS[0],
    };
  };

  return {
    stars,
    arms: Array.from({ length: armCount }, () => createParticle(false)),
    dust: Array.from({ length: dustCount }, () => createParticle(true)),
  };
}

function resizeCanvas(canvas: HTMLCanvasElement, width: number, height: number) {
  canvas.width = width;
  canvas.height = height;
}

function radialGlow(context: CanvasRenderingContext2D, x: number, y: number, radius: number, inner: string, outer: string) {
  const gradient = context.createRadialGradient(x, y, 0, x, y, radius);
  gradient.addColorStop(0, inner);
  gradient.addColorStop(1, outer);
  context.fillStyle = gradient;
  context.beginPath();
  context.arc(x, y, radius, 0, Math.PI * 2);
  context.fill();
}

const Galaxy = (props: GalaxyProps) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const settingsRef = useRef(props);
  const pointerRef = useRef({ x: 0, y: 0, targetX: 0, targetY: 0 });
  settingsRef.current = props;

  useEffect(() => {
    const container = containerRef.current;
    const canvas = canvasRef.current;
    const context = canvas?.getContext('2d', { alpha: false });
    if (!container || !canvas || !context) return undefined;

    const skyLayer = document.createElement('canvas');
    const galaxyLayer = document.createElement('canvas');
    const sky = skyLayer.getContext('2d', { alpha: false });
    const galaxy = galaxyLayer.getContext('2d');
    if (!sky || !galaxy) return undefined;

    let width = 1;
    let height = 1;
    let frameId = 0;
    let lastFrame = 0;
    const startTime = performance.now();
    const reducedMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;

    const rebuild = () => {
      const settings = settingsRef.current;
      width = Math.max(1, container.clientWidth);
      height = Math.max(1, container.clientHeight);
      // One device pixel per CSS pixel is enough for a moving star field and
      // avoids redrawing millions of extra pixels on high-DPI displays.
      resizeCanvas(canvas, width, height);
      resizeCanvas(skyLayer, width, height);
      resizeCanvas(galaxyLayer, width, height);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;

      const colors = settings.starColors?.length ? settings.starColors : COLORS;
      const field = createField(
        width,
        height,
        Math.max(0.4, settings.density ?? 1),
        colors,
        Math.max(3, Math.min(6, Math.round(settings.layers ?? 4))),
      );
      const opacity = Math.max(0, Math.min(1, settings.opacity ?? 1));
      const starScale = settings.starSize ?? 1;
      const glow = settings.glowIntensity ?? 0.25;
      const flareStrength = Math.max(0, settings.flareIntensity ?? 0.7);
      const background = settings.backgroundColor ?? '#02040a';

      sky.globalCompositeOperation = 'source-over';
      sky.globalAlpha = 1;
      sky.fillStyle = background;
      sky.fillRect(0, 0, width, height);
      for (const star of field.stars) {
        sky.globalAlpha = star.alpha * opacity;
        sky.fillStyle = star.color;
        sky.beginPath();
        sky.arc(star.x * width, star.y * height, Math.max(0.4, star.size * starScale), 0, Math.PI * 2);
        sky.fill();
        if (star.flare > 0.42 && flareStrength > 0) {
          const x = star.x * width;
          const y = star.y * height;
          const reach = (4 + star.flare * 11) * flareStrength * starScale;
          sky.save();
          sky.globalAlpha = star.alpha * star.flare * 0.58 * opacity;
          sky.strokeStyle = star.color;
          sky.lineWidth = Math.max(0.35, star.size * 0.28);
          sky.shadowColor = star.color;
          sky.shadowBlur = 7 + 10 * glow;
          sky.beginPath();
          sky.moveTo(x - reach, y);
          sky.lineTo(x + reach, y);
          sky.moveTo(x, y - reach * 0.72);
          sky.lineTo(x, y + reach * 0.72);
          sky.stroke();
          sky.restore();
        }
      }

      const centerX = width * 0.51;
      const centerY = height * 0.5;
      const radius = Math.max(width * 0.58, height * 0.72);
      const tilt = Math.min(0.44, Math.max(0.31, height / Math.max(width, 1) * 0.5));
      const cosTilt = Math.cos(-0.2);
      const sinTilt = Math.sin(-0.2);
      const project = (particle: Particle) => {
        const radial = radius * particle.radius;
        const rawX = Math.cos(particle.angle) * radial;
        const rawY = (Math.sin(particle.angle) * radial + particle.spread * radius) * tilt;
        return {
          x: centerX + rawX * cosTilt - rawY * sinTilt,
          y: centerY + rawX * sinTilt + rawY * cosTilt,
        };
      };

      galaxy.clearRect(0, 0, width, height);
      galaxy.globalCompositeOperation = 'lighter';
      radialGlow(galaxy, centerX, centerY, radius * 0.38, `rgba(91,112,255,${0.085 * opacity})`, 'rgba(35,45,125,0)');
      radialGlow(galaxy, centerX, centerY, radius * 0.13, `rgba(235,248,255,${0.34 * opacity})`, 'rgba(92,126,255,0)');

      // Expensive blur runs only when the viewport changes, not every frame.
      galaxy.filter = `blur(${Math.max(2, 5 * glow)}px)`;
      for (const particle of field.dust) {
        const point = project(particle);
        galaxy.globalAlpha = particle.alpha * opacity;
        galaxy.fillStyle = particle.color;
        galaxy.beginPath();
        galaxy.arc(point.x, point.y, particle.size * (0.55 + glow), 0, Math.PI * 2);
        galaxy.fill();
      }
      galaxy.filter = 'none';
      for (const particle of field.arms) {
        const point = project(particle);
        galaxy.globalAlpha = particle.alpha * (1.35 - particle.radius * 0.5) * opacity;
        galaxy.fillStyle = particle.color;
        galaxy.beginPath();
        galaxy.arc(point.x, point.y, particle.size * starScale, 0, Math.PI * 2);
        galaxy.fill();
      }
      galaxy.globalAlpha = 1;
      galaxy.globalCompositeOperation = 'source-over';
    };

    const draw = (now: number) => {
      const animated = !reducedMotion && !settingsRef.current.disableAnimation;
      if (animated) frameId = requestAnimationFrame(draw);
      if (animated && now - lastFrame < TARGET_FRAME_MS) return;
      lastFrame = now;
      const settings = settingsRef.current;
      const elapsed = reducedMotion || settings.disableAnimation ? 0 : (now - startTime) / 1000;
      const direction = settings.direction === 'backward' ? -1 : 1;
      const angle = elapsed * (settings.rotationSpeed ?? 0.035) * 0.18 * direction;

      pointerRef.current.x += (pointerRef.current.targetX - pointerRef.current.x) * 0.08;
      pointerRef.current.y += (pointerRef.current.targetY - pointerRef.current.y) * 0.08;
      const parallaxX = pointerRef.current.x * 13;
      const parallaxY = pointerRef.current.y * 9;

      context.globalCompositeOperation = 'source-over';
      context.globalAlpha = 1;
      context.drawImage(skyLayer, 0, 0);
      context.save();
      context.translate(width * 0.51 + parallaxX, height * 0.5 + parallaxY);
      context.rotate(angle);
      context.translate(-width * 0.51, -height * 0.5);
      context.drawImage(galaxyLayer, 0, 0);
      context.restore();
    };

    const onPointerMove = (event: PointerEvent) => {
      if (settingsRef.current.mouseInteraction === false) return;
      const rect = container.getBoundingClientRect();
      pointerRef.current.targetX = ((event.clientX - rect.left) / Math.max(1, rect.width) - 0.5) * 2;
      pointerRef.current.targetY = ((event.clientY - rect.top) / Math.max(1, rect.height) - 0.5) * 2;
    };
    const resizeObserver = new ResizeObserver(() => {
      rebuild();
      if (reducedMotion || settingsRef.current.disableAnimation) draw(performance.now());
    });
    resizeObserver.observe(container);
    container.addEventListener('pointermove', onPointerMove);
    rebuild();
    frameId = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(frameId);
      resizeObserver.disconnect();
      container.removeEventListener('pointermove', onPointerMove);
    };
  }, []);

  return (
    <div
      ref={containerRef}
      className={['relative h-full w-full overflow-hidden', props.className].filter(Boolean).join(' ')}
      style={props.style}
      aria-hidden="true"
    >
      <canvas ref={canvasRef} className="absolute inset-0 block h-full w-full" />
    </div>
  );
};

export default Galaxy;
