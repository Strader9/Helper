import { useEffect, useRef, type HTMLAttributes } from 'react';

export type GalaxyDirection = 'forward' | 'backward';

export interface GalaxyProps extends HTMLAttributes<HTMLDivElement> {
  focal?: [number, number];
  rotation?: [number, number];
  starSpeed?: number;
  density?: number;
  hueShift?: number;
  disableAnimation?: boolean;
  speed?: number;
  mouseInteraction?: boolean;
  glowIntensity?: number;
  saturation?: number;
  mouseRepulsion?: boolean;
  repulsionStrength?: number;
  twinkleIntensity?: number;
  rotationSpeed?: number;
  autoCenterRepulsion?: number;
  transparent?: boolean;
  lightMode?: boolean;
  starColors?: string[];
  starSize?: number;
  flareIntensity?: number;
  layers?: number;
  direction?: GalaxyDirection;
  clickWarp?: boolean;
  intro?: boolean;
  backgroundColor?: string;
  opacity?: number;
  dpr?: number;
}

type FarStar = { x: number; y: number; size: number; alpha: number; phase: number; color: string };
type ArmParticle = { radius: number; angle: number; spread: number; size: number; alpha: number; phase: number; color: string };
type GalaxyField = { farStars: FarStar[]; armParticles: ArmParticle[]; dustParticles: ArmParticle[] };

const DEFAULT_COLORS = ['#f7fbff', '#a7dcff', '#8d9cff', '#b69cff'];

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

function createField(width: number, height: number, density: number, colors: string[], arms: number): GalaxyField {
  const random = seededRandom(Math.round(width * 17 + height * 31 + density * 997));
  const areaScale = Math.min(1.5, Math.max(0.72, (width * height) / (1440 * 900)));
  const farCount = Math.round(360 * density * areaScale);
  const armCount = Math.round(1850 * density * areaScale);
  const dustCount = Math.round(620 * density * areaScale);

  const farStars = Array.from({ length: farCount }, (): FarStar => ({
    x: random(),
    y: random(),
    size: 0.35 + Math.pow(random(), 5) * 1.75,
    alpha: 0.16 + random() * 0.62,
    phase: random() * Math.PI * 2,
    color: colors[Math.floor(random() * colors.length)] ?? DEFAULT_COLORS[0],
  }));

  const makeParticle = (dust: boolean): ArmParticle => {
    const radius = Math.pow(random(), dust ? 0.78 : 0.62);
    const arm = Math.floor(random() * arms);
    const angle = (arm / arms) * Math.PI * 2 + radius * Math.PI * 3.65 + gaussian(random) * (0.045 + radius * 0.19);
    return {
      radius,
      angle,
      spread: gaussian(random) * (dust ? 0.12 : 0.07) * (0.25 + radius),
      size: dust ? 3 + random() * 11 : 0.32 + Math.pow(random(), 3) * 2.1,
      alpha: dust ? 0.008 + random() * 0.024 : 0.11 + random() * 0.72,
      phase: random() * Math.PI * 2,
      color: colors[Math.floor(random() * colors.length)] ?? DEFAULT_COLORS[0],
    };
  };

  return {
    farStars,
    armParticles: Array.from({ length: armCount }, () => makeParticle(false)),
    dustParticles: Array.from({ length: dustCount }, () => makeParticle(true)),
  };
}

function drawGlow(context: CanvasRenderingContext2D, x: number, y: number, radius: number, inner: string, outer: string) {
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

    let width = 1;
    let height = 1;
    let field: GalaxyField = { farStars: [], armParticles: [], dustParticles: [] };
    let frameId = 0;
    const startTime = performance.now();
    const reduceMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;

    const rebuild = () => {
      const settings = settingsRef.current;
      width = Math.max(1, container.clientWidth);
      height = Math.max(1, container.clientHeight);
      const ratio = Math.min(settings.dpr ?? window.devicePixelRatio ?? 1, 1.5);
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      field = createField(
        width,
        height,
        Math.max(0.4, settings.density ?? 1),
        settings.starColors?.length ? settings.starColors : DEFAULT_COLORS,
        Math.max(3, Math.min(6, Math.round(settings.layers ?? 4))),
      );
    };

    const draw = (now: number) => {
      const settings = settingsRef.current;
      const elapsed = reduceMotion || settings.disableAnimation ? 0 : (now - startTime) / 1000;
      const direction = settings.direction === 'backward' ? -1 : 1;
      const rotation = elapsed * (settings.rotationSpeed ?? 0.035) * direction;
      const drift = elapsed * (settings.starSpeed ?? 0.35) * 0.012;
      const opacity = Math.max(0, Math.min(1, settings.opacity ?? 1));
      const starScale = settings.starSize ?? 1;
      const twinkle = settings.twinkleIntensity ?? 0.3;
      const glow = settings.glowIntensity ?? 0.25;
      const background = settings.backgroundColor ?? '#02040a';

      pointerRef.current.x += (pointerRef.current.targetX - pointerRef.current.x) * 0.035;
      pointerRef.current.y += (pointerRef.current.targetY - pointerRef.current.y) * 0.035;
      const parallaxX = pointerRef.current.x * 18;
      const parallaxY = pointerRef.current.y * 12;

      context.globalCompositeOperation = 'source-over';
      context.globalAlpha = 1;
      context.fillStyle = background;
      context.fillRect(0, 0, width, height);

      for (const star of field.farStars) {
        const x = ((star.x + drift * (0.15 + star.size * 0.08)) % 1) * width + parallaxX * star.size * 0.08;
        const y = star.y * height + parallaxY * star.size * 0.08;
        const shimmer = 1 - twinkle * 0.45 + Math.sin(elapsed * 0.9 + star.phase) * twinkle * 0.45;
        context.globalAlpha = star.alpha * shimmer * opacity;
        context.fillStyle = star.color;
        context.beginPath();
        context.arc(x, y, Math.max(0.35, star.size * starScale), 0, Math.PI * 2);
        context.fill();
      }

      const centerX = width * 0.51 + parallaxX;
      const centerY = height * 0.5 + parallaxY;
      const galaxyRadius = Math.max(width * 0.58, height * 0.72);
      const tilt = Math.min(0.44, Math.max(0.31, height / Math.max(width, 1) * 0.5));
      const cosTilt = Math.cos(-0.2);
      const sinTilt = Math.sin(-0.2);

      context.globalCompositeOperation = 'lighter';
      drawGlow(context, centerX, centerY, galaxyRadius * 0.34, `rgba(91,112,255,${0.045 * opacity})`, 'rgba(35,45,125,0)');
      drawGlow(context, centerX, centerY, galaxyRadius * 0.1, `rgba(225,242,255,${0.2 * opacity})`, 'rgba(92,126,255,0)');

      const project = (particle: ArmParticle) => {
        const angle = particle.angle + rotation * (1.3 - particle.radius * 0.55);
        const radial = galaxyRadius * particle.radius;
        const rawX = Math.cos(angle) * radial;
        const rawY = (Math.sin(angle) * radial + particle.spread * galaxyRadius) * tilt;
        return {
          x: centerX + rawX * cosTilt - rawY * sinTilt,
          y: centerY + rawX * sinTilt + rawY * cosTilt,
        };
      };

      context.filter = `blur(${Math.max(2, 5 * glow)}px)`;
      for (const particle of field.dustParticles) {
        const point = project(particle);
        context.globalAlpha = particle.alpha * opacity;
        context.fillStyle = particle.color;
        context.beginPath();
        context.arc(point.x, point.y, particle.size * (0.55 + glow), 0, Math.PI * 2);
        context.fill();
      }
      context.filter = 'none';

      for (const particle of field.armParticles) {
        const point = project(particle);
        const shimmer = 0.72 + Math.sin(elapsed * (0.55 + (settings.speed ?? 0.7) * 0.2) + particle.phase) * twinkle * 0.32;
        const coreBoost = 1.35 - particle.radius * 0.5;
        context.globalAlpha = particle.alpha * shimmer * coreBoost * opacity;
        context.fillStyle = particle.color;
        context.beginPath();
        context.arc(point.x, point.y, particle.size * starScale, 0, Math.PI * 2);
        context.fill();
      }

      context.globalCompositeOperation = 'source-over';
      context.globalAlpha = 1;
      if (!reduceMotion && !settings.disableAnimation) frameId = requestAnimationFrame(draw);
    };

    const onPointerMove = (event: PointerEvent) => {
      if (settingsRef.current.mouseInteraction === false) return;
      const rect = container.getBoundingClientRect();
      pointerRef.current.targetX = ((event.clientX - rect.left) / Math.max(1, rect.width) - 0.5) * 2;
      pointerRef.current.targetY = ((event.clientY - rect.top) / Math.max(1, rect.height) - 0.5) * 2;
    };

    const resizeObserver = new ResizeObserver(() => {
      rebuild();
      if (reduceMotion || settingsRef.current.disableAnimation) draw(performance.now());
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
