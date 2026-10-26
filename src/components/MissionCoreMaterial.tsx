import { useEffect, useRef } from 'react';

export type MissionTheme = 'oscillation' | 'nebula' | 'cyber';

const MATRIX_GLYPHS = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZアイウエオカキクケコサシスセソ<>/{}[]';

function MatrixRain() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = canvas?.parentElement;
    const context = canvas?.getContext('2d');
    if (!canvas || !container || !context) return undefined;

    let frame = 0;
    let lastFrame = 0;
    let columns: Array<{ progress: number; speed: number; phase: number }> = [];
    const pointer = { x: 0.5, y: 0.5, active: false, pulse: 0 };

    const resize = () => {
      const rect = container.getBoundingClientRect();
      const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
      canvas.width = Math.max(1, Math.round(rect.width * dpr));
      canvas.height = Math.max(1, Math.round(rect.height * dpr));
      canvas.style.width = `${rect.width}px`;
      canvas.style.height = `${rect.height}px`;
      context.setTransform(dpr, 0, 0, dpr, 0, 0);
      const count = Math.max(12, Math.floor(rect.width / 13));
      columns = Array.from({ length: count }, (_, index) => ({
        progress: Math.random(),
        speed: 0.085 + Math.random() * 0.075,
        phase: index * 0.73 + Math.random() * 8,
      }));
    };

    const render = (now: number) => {
      frame = requestAnimationFrame(render);
      if (now - lastFrame < 1000 / 30) return;
      const dt = Math.min(0.05, (now - (lastFrame || now - 16)) / 1000);
      lastFrame = now;
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      context.clearRect(0, 0, width, height);
      context.font = '700 11px ui-monospace, SFMono-Regular, Consolas, monospace';
      context.textAlign = 'center';
      context.textBaseline = 'middle';
      context.globalCompositeOperation = 'lighter';

      const step = width / columns.length;
      pointer.pulse *= 0.94;
      columns.forEach((column, index) => {
        const baseX = (index + 0.5) * step;
        const dx = baseX / width - pointer.x;
        const influence = pointer.active ? Math.exp(-(dx * dx) / 0.035) : 0;
        const acceleration = 1 + influence * 2.4 + pointer.pulse * 1.8;
        column.progress = (column.progress + column.speed * acceleration * dt) % 1;
        const travel = height + 146;

        for (let stream = 0; stream < 2; stream += 1) {
          const streamProgress = (column.progress + stream * 0.53) % 1;
          const headY = -56 + streamProgress * travel;

          for (let row = 0; row < 16; row += 1) {
            const y = headY - row * 13;
            if (y < -14 || y > height + 14) continue;
            const seed = Math.floor(now / 110 + column.phase * 17 + row * 7 + index * 13 + stream * 29);
            const glyph = MATRIX_GLYPHS[seed % MATRIX_GLYPHS.length];
            const fade = Math.max(0, 1 - row / 16);
            const pointerY = pointer.y * height;
            const proximity = pointer.active ? Math.exp(-((y - pointerY) ** 2) / 1800) * influence : 0;
            context.globalAlpha = Math.min(1, 0.06 + fade * 0.66 + proximity * 0.52);
            context.fillStyle = row === 0 ? '#d8ffe6' : proximity > 0.18 ? '#8dffb5' : '#35ff82';
            context.shadowColor = '#35ff82';
            context.shadowBlur = row === 0 ? 13 : 4 + proximity * 12;
            const drift = Math.sin(now * 0.0014 + column.phase + row * 0.22 + stream) * (1.2 + influence * 4.5);
            context.fillText(glyph, baseX + drift, y);
          }
        }
      });
      context.globalAlpha = 1;
      context.shadowBlur = 0;
      context.globalCompositeOperation = 'source-over';
    };

    const onPointerMove = (event: PointerEvent) => {
      const rect = container.getBoundingClientRect();
      pointer.x = (event.clientX - rect.left) / Math.max(rect.width, 1);
      pointer.y = (event.clientY - rect.top) / Math.max(rect.height, 1);
      pointer.active = true;
    };
    const onPointerLeave = () => { pointer.active = false; };
    const onPointerDown = () => { pointer.pulse = 1; };

    const observer = new ResizeObserver(resize);
    observer.observe(container);
    resize();
    frame = requestAnimationFrame(render);
    container.addEventListener('pointermove', onPointerMove);
    container.addEventListener('pointerleave', onPointerLeave);
    container.addEventListener('pointerdown', onPointerDown);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      container.removeEventListener('pointermove', onPointerMove);
      container.removeEventListener('pointerleave', onPointerLeave);
      container.removeEventListener('pointerdown', onPointerDown);
    };
  }, []);

  return <canvas ref={canvasRef} className="mission-core-matrix-rain" aria-hidden="true" />;
}

export function MissionCoreMaterial({ theme }: { theme: Exclude<MissionTheme, 'nebula'> }) {
  return (
    <div className={`mission-core-material mission-core-material--${theme}`}>
      {theme === 'oscillation' ? (
        <div className="mission-core-sand" aria-hidden="true">
          <span className="mission-core-sand__body" />
          <span className="mission-core-sand__grain" />
          <span className="mission-core-sand__dust" />
          <span className="mission-core-sand__light" />
        </div>
      ) : (
        <MatrixRain />
      )}
    </div>
  );
}
