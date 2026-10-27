import React, { useEffect, useRef, useState } from 'react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';

const DAYLIGHT_CORE_PALETTE = {
  ink: '#25313a',
  mineralBlue: '#2d5f88',
  warmCopper: '#b7792d',
  deepCopper: '#8f5a24',
  jadeInk: '#326f63',
};

export type EAssistantCoreVoiceState = 'idle' | 'connecting' | 'listening' | 'thinking' | 'speaking' | 'queued' | 'passive';

type OrbParticle = {
  x: number;
  y: number;
  z: number;
  baseX: number;
  baseY: number;
  baseZ: number;
  color: string;
  size: number;
  type: 'signal' | 'core' | 'void';
  phase: number;
  turbulence: number;
  shell: number;
};

const SPHERE_RADIUS = 180;

function daylightParticleColor(color: string) {
  if (color === '#ffffff') return DAYLIGHT_CORE_PALETTE.mineralBlue;
  if (color === '#ff4d4d') return DAYLIGHT_CORE_PALETTE.warmCopper;
  if (color === '#ffcc00') return DAYLIGHT_CORE_PALETTE.deepCopper;
  if (color.startsWith('hsl')) return DAYLIGHT_CORE_PALETTE.jadeInk;
  return color;
}

function daylightVoidColor(x: number, y: number, z: number) {
  const phase = Math.sin((x + y + z) * 0.045);
  return phase > 0.34
    ? DAYLIGHT_CORE_PALETTE.mineralBlue
    : phase < -0.34
      ? DAYLIGHT_CORE_PALETTE.warmCopper
      : DAYLIGHT_CORE_PALETTE.ink;
}

function createParticle(): OrbParticle {
  const theta = 2 * Math.PI * Math.random();
  const phi = Math.acos(2 * Math.random() - 1);
  // Most points sit close to the photosphere so this reads as a single star,
  // not as a loose particle cloud.
  const shell = Math.random();
  const radius = SPHERE_RADIUS * (shell < 0.82
    ? 0.76 + Math.pow(Math.random(), 0.42) * 0.24
    : 0.22 + Math.random() * 0.62);
  const baseX = radius * Math.sin(phi) * Math.cos(theta);
  const baseY = radius * Math.sin(phi) * Math.sin(theta);
  const baseZ = radius * Math.cos(phi);
  const kind = Math.random();
  const type = kind < 0.4 ? 'signal' : kind < 0.8 ? 'core' : 'void';
  return {
    x: baseX,
    y: baseY,
    z: baseZ,
    baseX,
    baseY,
    baseZ,
    type,
    color: type === 'signal' ? '#ff4d4d' : type === 'core' ? '#ffffff' : '#000000',
    size: Math.random() * 1.55 + 0.45,
    phase: Math.random() * Math.PI * 2,
    turbulence: 0.55 + Math.random() * 1.25,
    shell: radius / SPHERE_RADIUS,
  };
}

function updateParticle(
  particle: OrbParticle,
  time: number,
  rotX: number,
  rotY: number,
  callState: EAssistantCoreVoiceState,
  audioLevel: number,
) {
  const agitation = callState === 'thinking' || callState === 'speaking'
    ? 1.55
    : callState === 'listening' || callState === 'connecting'
      ? 1.28
      : 1;
  if (particle.type === 'signal') {
    particle.color = callState === 'listening'
      ? '#ffb86b'
      : callState === 'speaking'
        ? '#fff7ed'
        : '#66d9ff';
  } else if (particle.type === 'core') {
    particle.color = particle.shell > 0.84 ? '#dff8ff' : '#8ba7ff';
  }
  const surfaceWave = Math.sin(time * 0.0032 * particle.turbulence + particle.phase)
    + Math.sin(time * 0.0018 + particle.baseX * 0.034 - particle.baseY * 0.021 + particle.phase) * 0.72
    + Math.cos(time * 0.0026 + particle.baseZ * 0.046 - particle.phase) * 0.42;
  const eruption = Math.pow(Math.max(0, Math.sin(time * 0.0011 + particle.phase * 2.7)), 7)
    * (particle.shell > 0.86 ? 20 : 4);
  const wave = (surfaceWave * (7 + particle.shell * 9) + eruption + audioLevel * 48)
    * agitation;
  const radialUnit = Math.sqrt(particle.baseX ** 2 + particle.baseY ** 2 + particle.baseZ ** 2) + 0.001;
  const radialFactor = (radialUnit + wave) / radialUnit;
  let x = particle.baseX * radialFactor;
  let y = particle.baseY * radialFactor;
  let z = particle.baseZ * radialFactor;
  const shear = Math.sin(time * 0.0015 + particle.baseY * 0.014 + particle.phase)
    * particle.turbulence * agitation;
  x += Math.cos(particle.phase + time * 0.0012) * shear * 3.2;
  z += Math.sin(particle.phase + time * 0.0014) * shear * 3.2;
  const cosX = Math.cos(rotX);
  const sinX = Math.sin(rotX);
  const rotatedY = y * cosX - z * sinX;
  const rotatedZ = y * sinX + z * cosX;
  y = rotatedY;
  z = rotatedZ;
  const cosY = Math.cos(rotY);
  const sinY = Math.sin(rotY);
  particle.x = x * cosY + z * sinY;
  particle.z = -x * sinY + z * cosY;
  particle.y = y;
}

function drawParticle(
  context: CanvasRenderingContext2D,
  particle: OrbParticle,
  centerX: number,
  centerY: number,
  isLightMode: boolean,
) {
  const boundedZ = Math.max(-300, Math.min(particle.z, 590));
  const perspective = 600 / (600 - boundedZ);
  const x = particle.x * perspective + centerX;
  const y = particle.y * perspective + centerY;
  const size = particle.size * perspective * (isLightMode ? 1.16 : 1);

  if (particle.type === 'void') {
    if (isLightMode) {
      const color = daylightVoidColor(particle.baseX, particle.baseY, particle.baseZ);
      context.fillStyle = color;
      context.shadowColor = color;
      context.shadowBlur = 2.5;
      context.globalAlpha = Math.max(0.26, perspective - 0.3);
      context.beginPath();
      context.arc(x, y, size, 0, Math.PI * 2);
      context.fill();
      context.shadowBlur = 0;
    } else {
      context.globalAlpha = Math.max(0.08, (perspective - 0.46) * 0.42);
      context.fillStyle = particle.shell > 0.8 ? '#7182c5' : '#1c2445';
      context.beginPath();
      context.arc(x, y, size, 0, Math.PI * 2);
      context.fill();
    }
    return;
  }

  const color = isLightMode ? daylightParticleColor(particle.color) : particle.color;
  if (isLightMode) {
    context.shadowColor = color;
    context.shadowBlur = 2;
  }
  context.fillStyle = color;
  context.globalAlpha = Math.max(isLightMode ? 0.3 : 0.14, perspective - (isLightMode ? 0.3 : 0.34));
  context.beginPath();
  context.arc(x, y, size, 0, Math.PI * 2);
  context.fill();
  context.shadowBlur = 0;
}

function drawStellarBody(
  context: CanvasRenderingContext2D,
  centerX: number,
  centerY: number,
  time: number,
  state: EAssistantCoreVoiceState,
  audioLevel: number,
  isLightMode: boolean,
) {
  if (isLightMode) return;
  const active = state === 'thinking' || state === 'speaking' ? 1.35 : state === 'listening' ? 1.18 : 1;
  const pulse = (Math.sin(time * 0.0027) * 0.5 + 0.5) * active + audioLevel * 1.4;
  const radius = 171 + pulse * 7;

  context.save();
  context.globalCompositeOperation = 'lighter';
  const corona = context.createRadialGradient(centerX, centerY, radius * 0.42, centerX, centerY, radius * 1.52);
  corona.addColorStop(0, 'rgba(112, 142, 255, 0.10)');
  corona.addColorStop(0.43, `rgba(76, 128, 255, ${(0.11 + pulse * 0.018).toFixed(3)})`);
  corona.addColorStop(0.7, `rgba(67, 211, 255, ${(0.07 + pulse * 0.012).toFixed(3)})`);
  corona.addColorStop(1, 'rgba(52, 95, 210, 0)');
  context.fillStyle = corona;
  context.beginPath();
  context.arc(centerX, centerY, radius * 1.55, 0, Math.PI * 2);
  context.fill();

  const body = context.createRadialGradient(
    centerX - radius * 0.24,
    centerY - radius * 0.27,
    radius * 0.03,
    centerX,
    centerY,
    radius,
  );
  body.addColorStop(0, 'rgba(241, 250, 255, 0.34)');
  body.addColorStop(0.18, 'rgba(156, 204, 255, 0.22)');
  body.addColorStop(0.56, 'rgba(74, 112, 224, 0.15)');
  body.addColorStop(0.86, 'rgba(67, 216, 255, 0.13)');
  body.addColorStop(1, 'rgba(44, 72, 176, 0.02)');
  context.fillStyle = body;
  context.beginPath();
  context.arc(centerX, centerY, radius, 0, Math.PI * 2);
  context.fill();

  for (let index = 0; index < 5; index += 1) {
    const phase = time * (0.00018 + index * 0.000025) + index * 1.21;
    const arcRadius = radius * (1.02 + 0.045 * Math.sin(time * 0.0017 + index));
    context.save();
    context.translate(centerX, centerY);
    context.rotate(phase);
    context.scale(1, 0.72 + index * 0.035);
    context.strokeStyle = index % 2
      ? `rgba(115, 225, 255, ${(0.10 + pulse * 0.012).toFixed(3)})`
      : `rgba(139, 126, 255, ${(0.085 + pulse * 0.01).toFixed(3)})`;
    context.lineWidth = 1.2 + (index % 3) * 0.55;
    context.shadowColor = index % 2 ? '#6ee7ff' : '#8b7dff';
    context.shadowBlur = 10;
    context.beginPath();
    context.arc(0, 0, arcRadius, -0.72, 0.78);
    context.stroke();
    context.restore();
  }
  context.restore();
}

export function EAssistantCoreOrb({
  sentiment = 'default',
  callState = 'idle',
  audioLevel = 0,
  highPerformance = false,
  reaction,
  facePresent = false,
  isLightMode = false,
  className = '',
}: {
  sentiment?: 'default' | 'excited' | 'focused' | 'zen';
  callState?: EAssistantCoreVoiceState;
  audioLevel?: number;
  highPerformance?: boolean;
  reaction?: string | null;
  facePresent?: boolean;
  isLightMode?: boolean;
  className?: string;
}) {
  const reducedMotion = useReducedMotion();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const particlesRef = useRef<OrbParticle[]>([]);
  const pointerRef = useRef({ x: 0, y: 0, dragging: false });
  const rotationRef = useRef({ x: 0, y: 0 });
  const callStateRef = useRef(callState);
  const sentimentRef = useRef(sentiment);
  const audioLevelRef = useRef(audioLevel);
  const lightModeRef = useRef(isLightMode);
  const facePresentRef = useRef(facePresent);
  const [interactionPulse, setInteractionPulse] = useState(0);
  const [reactionColor, setReactionColor] = useState('rgba(255,200,80,0.2)');

  useEffect(() => { callStateRef.current = callState; }, [callState]);
  useEffect(() => { sentimentRef.current = sentiment; }, [sentiment]);
  useEffect(() => { audioLevelRef.current = audioLevel; }, [audioLevel]);
  useEffect(() => { lightModeRef.current = isLightMode; }, [isLightMode]);
  useEffect(() => { facePresentRef.current = facePresent; }, [facePresent]);
  useEffect(() => {
    const onAudioLevel = (event: Event) => {
      const level = Number((event as CustomEvent<{ level?: number }>).detail?.level);
      if (Number.isFinite(level)) audioLevelRef.current = level;
    };
    window.addEventListener('e-assistant:voice-audio-level', onAudioLevel);
    return () => window.removeEventListener('e-assistant:voice-audio-level', onAudioLevel);
  }, []);
  useEffect(() => {
    if (!reaction) return;
    setInteractionPulse(value => value + 1);
    setReactionColor(isLightMode
      ? reaction === 'failed' ? 'rgba(143,90,36,0.22)' : reaction === 'jump' ? 'rgba(50,111,99,0.20)' : 'rgba(183,121,45,0.18)'
      : reaction === 'failed' ? 'rgba(255,60,60,0.25)' : reaction === 'jump' ? 'rgba(80,255,120,0.2)' : 'rgba(255,200,80,0.2)');
  }, [isLightMode, reaction]);

  const particleCount = reducedMotion ? 280 : highPerformance ? 900 : 520;
  useEffect(() => {
    particlesRef.current = Array.from({ length: particleCount }, createParticle);
  }, [particleCount]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const context = canvas.getContext('2d');
    if (!context) return;
    let animationFrame: number | undefined;
    let lastRenderedAt = 0;
    let renderedFrames = 0;

    const render = (time: number) => {
      if (!reducedMotion && time - lastRenderedAt < 1000 / 30) {
        animationFrame = requestAnimationFrame(render);
        return;
      }
      lastRenderedAt = time;
      renderedFrames += 1;
      try {
        context.clearRect(0, 0, canvas.width, canvas.height);
        const centerX = canvas.width / 2;
        const centerY = canvas.height / 2;
        const state = callStateRef.current;
        drawStellarBody(
          context,
          centerX,
          centerY,
          reducedMotion ? 0 : time,
          state,
          reducedMotion ? 0 : audioLevelRef.current,
          lightModeRef.current,
        );
        if (!reducedMotion && !pointerRef.current.dragging) {
          const speed = state === 'thinking'
            ? 4
            : sentimentRef.current === 'excited'
              ? 3
              : sentimentRef.current === 'focused'
                ? 2
                : sentimentRef.current === 'zen'
                  ? 0.5
                  : 1;
          rotationRef.current.y += 0.005 * speed;
          rotationRef.current.x += 0.002 * speed;
        }
        if (facePresentRef.current) {
          const pulse = 0.06 + Math.sin(time * 0.003) * 0.02;
          const glow = context.createRadialGradient(centerX, centerY, 100, centerX, centerY, 260);
          if (lightModeRef.current) {
            glow.addColorStop(0, `rgba(45,95,136,${(pulse * 0.58).toFixed(3)})`);
            glow.addColorStop(0.5, `rgba(183,121,45,${(pulse * 0.28).toFixed(3)})`);
            glow.addColorStop(1, 'rgba(45,95,136,0)');
          } else {
            glow.addColorStop(0, `rgba(255,200,100,${pulse.toFixed(3)})`);
            glow.addColorStop(0.5, `rgba(255,180,60,${(pulse * 0.5).toFixed(3)})`);
            glow.addColorStop(1, 'rgba(255,150,30,0)');
          }
          context.fillStyle = glow;
          context.fillRect(centerX - 260, centerY - 260, 520, 520);
        }
        for (const particle of particlesRef.current) {
          updateParticle(
            particle,
            reducedMotion ? 0 : time,
            rotationRef.current.x,
            rotationRef.current.y,
            state,
            reducedMotion ? 0 : audioLevelRef.current,
          );
        }
        // Depth order changes gradually, so recalculating it every third frame
        // looks identical while avoiding an O(n log n) sort on every tick.
        if (renderedFrames % 3 === 0) particlesRef.current.sort((left, right) => left.z - right.z);
        context.save();
        if (!lightModeRef.current) context.globalCompositeOperation = 'lighter';
        for (const particle of particlesRef.current) {
          drawParticle(context, particle, centerX, centerY, lightModeRef.current);
        }
        context.restore();
        context.globalAlpha = 1;
      } catch {
        // A single bad frame must not terminate EAssistant's visual heartbeat.
      }
      if (!reducedMotion) animationFrame = requestAnimationFrame(render);
    };

    if (reducedMotion) render(0);
    else animationFrame = requestAnimationFrame(render);
    return () => {
      if (animationFrame !== undefined) cancelAnimationFrame(animationFrame);
    };
  }, [reducedMotion]);

  const beginDrag = (event: React.MouseEvent | React.TouchEvent) => {
    const point = 'touches' in event ? event.touches[0] : event;
    pointerRef.current = { x: point.clientX, y: point.clientY, dragging: true };
  };
  const continueDrag = (event: React.MouseEvent | React.TouchEvent) => {
    if (!pointerRef.current.dragging) return;
    const point = 'touches' in event ? event.touches[0] : event;
    rotationRef.current.y += (point.clientX - pointerRef.current.x) * 0.01;
    rotationRef.current.x -= (point.clientY - pointerRef.current.y) * 0.01;
    pointerRef.current.x = point.clientX;
    pointerRef.current.y = point.clientY;
  };
  const endDrag = () => { pointerRef.current.dragging = false; };

  return (
    <div
      data-eassistant-core-orb
      data-performance={highPerformance ? 'high' : 'balanced'}
      data-reduced-motion={reducedMotion ? 'true' : 'false'}
      className={`relative flex aspect-square items-center justify-center cursor-grab active:cursor-grabbing ${className}`}
      onMouseDown={beginDrag}
      onMouseMove={continueDrag}
      onMouseUp={endDrag}
      onMouseLeave={endDrag}
      onTouchStart={beginDrag}
      onTouchMove={continueDrag}
      onTouchEnd={endDrag}
      onClick={() => setInteractionPulse(value => value + 1)}
    >
      <canvas ref={canvasRef} width={600} height={600} className="pointer-events-none relative z-10 h-full w-full" />
      <AnimatePresence>
        {[0, 1].map(index => (
          <motion.div
            key={`${interactionPulse}-${index}`}
            className="pointer-events-none absolute inset-0 rounded-full border will-change-transform"
            style={{ borderColor: reactionColor }}
            initial={{ scale: 0.5, opacity: 0 }}
            animate={{ scale: 1.5, opacity: 0 }}
            transition={{ duration: reducedMotion ? 0 : 1.5, delay: reducedMotion ? 0 : index * 0.3 }}
          />
        ))}
      </AnimatePresence>
    </div>
  );
}
