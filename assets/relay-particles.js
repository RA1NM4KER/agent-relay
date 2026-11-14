"use strict";
(() => {
  const art = document.querySelector('.relay-art');
  if (!art) return;
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  const phone = matchMedia('(max-width: 750px)');
  const theme = document.documentElement;
  const canvas = document.createElement('canvas');
  canvas.className = 'relay-particles';
  canvas.setAttribute('aria-hidden', 'true');
  const context = canvas.getContext('2d');
  if (!context) return;
  art.prepend(canvas);
  const count = innerWidth < 600 ? 1300 : 2400;
  const particles = Array.from({ length: count }, (_, i) => {
    const group = i % 2;
    const u = (i / count) * Math.PI * 4;
    const v = i * 2.3999632297;
    const radius = 0.17 + Math.sin(i * 12.7) * .025;
    const ring = 0.95 + radius * Math.cos(v);
    let x = ring * Math.cos(u);
    const y = ring * Math.sin(u) * 1.26;
    let z = radius * Math.sin(v);
    if (group) { const before = x; x = x * .34 + z * .94; z = -before * .94 + z * .34; }
    x += group ? .52 : -.52;
    const chainY = y + (group ? .56 : -.56);
    const angle = -.48;
    return {
      x: x * Math.cos(angle) - chainY * Math.sin(angle),
      y: x * Math.sin(angle) + chainY * Math.cos(angle), z,
      u, v, group, size: .55 + ((i * 37) % 100) / 100,
      drift: Math.sin(i * 23.13),
    };
  });
  let width = 0;
  let height = 0;
  let ratio = 1;
  let running = false;
  let inView = false;
  let request;
  let lastFrame = 0;
  let elapsed = 0;
  let pointer = { x: 0, y: 0, active: false };
  let turnX = 0;
  let turnY = 0;
  function resize() {
    const bounds = art.getBoundingClientRect();
    if (bounds.width <= 0) return;
    width = bounds.width;
    height = width * 1.04;
    ratio = Math.min(devicePixelRatio || 1, 1.5);
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    canvas.style.height = `${height}px`;
    sync();
  }
  function draw(now, still = false) {
    if ((!running && !still) || !width || !height) return;
    if (!still) {
      request = requestAnimationFrame(draw);
      if (now - lastFrame < 32) return;
      const delta = lastFrame ? Math.min(now - lastFrame, 80) : 32;
      elapsed += delta / 1000;
      lastFrame = now;
    }
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);
    const light = theme.dataset.theme === 'light';
    const period = elapsed % 22;
    // Spend time on each resolved form, then carry the particles into the next one.
    let morph = period < 7 ? 0 : period < 11 ? (period - 7) / 4 : period < 16 ? 1 : period < 20 ? 1 - (period - 16) / 4 : 0;
    morph = morph * morph * (3 - 2 * morph);
    turnX += ((pointer.active ? pointer.y * .16 : .1) - turnX) * .06;
    turnY += ((pointer.active ? pointer.x * .24 : 0) - turnY) * .06;
    const ay = Math.sin(elapsed * .18) * .3 + turnY;
    const az = Math.sin(elapsed * .12) * .08;
    const scale = width * .245;
    const cosY = Math.cos(ay), sinY = Math.sin(ay);
    const cosX = Math.cos(turnX), sinX = Math.sin(turnX);
    const cosZ = Math.cos(az), sinZ = Math.sin(az);
    for (const p of particles) {
      const t = p.u + elapsed * .09;
      const tube = .18;
      const flowX = 1.5 * Math.sin(t) + tube * Math.cos(p.v) * Math.cos(t);
      const flowY = .7 * Math.sin(t * 2) + tube * Math.sin(p.v);
      const flowZ = .65 * Math.cos(t) + tube * Math.cos(p.v);
      const spread = Math.sin(morph * Math.PI) * .12 * p.drift;
      const x = p.x * (1 - morph) + flowX * morph + spread;
      const y = p.y * (1 - morph) + flowY * morph;
      const z = p.z * (1 - morph) + flowZ * morph;
      const rx = x * cosY + z * sinY;
      const rz = -x * sinY + z * cosY;
      const ry = y * cosX - rz * sinX;
      const depth = y * sinX + rz * cosX;
      const perspective = 4.5 / (4.5 - depth);
      let px = width * .5 + (rx * cosZ - ry * sinZ) * scale * perspective;
      let py = height * .49 + (rx * sinZ + ry * cosZ) * scale * perspective;
      if (pointer.active) {
        const dx = px - (pointer.x * .5 + .5) * width;
        const dy = py - (pointer.y * .5 + .5) * height;
        const force = Math.max(0, 1 - Math.hypot(dx, dy) / (width * .23));
        px += dx * force * .28;
        py += dy * force * .28;
      }
      const alpha = Math.min(1, Math.max(.18, .62 + depth * .18));
      const shimmer = (Math.sin(p.u * 3 + elapsed * .6) + 1) * .5;
      context.globalAlpha = alpha;
      context.fillStyle = p.group ? (light ? '#74614b' : (shimmer > .75 ? '#f0e4d1' : '#b9b1a5')) : (light ? '#95541c' : (shimmer > .7 ? '#ffe3ad' : '#e8b66f'));
      const size = p.size * perspective * Math.max(.75, width / 500);
      context.beginPath();context.arc(px, py, size, 0, Math.PI * 2);context.fill();
    }
    context.globalAlpha = 1;
    art.classList.add('particles-ready');
  }
  function sync() {
    const next = inView && !phone.matches && !motion.matches && !document.hidden;
    if (inView && !phone.matches && motion.matches && !document.hidden) {
      running = false;
      cancelAnimationFrame(request);
      lastFrame = 0;
      draw(performance.now(), true);
      return;
    }
    if (next === running) return;
    running = next;
    cancelAnimationFrame(request);
    lastFrame = 0;
    if (running) request = requestAnimationFrame(draw);
  }
  canvas.addEventListener('pointermove', event => {
    const bounds = canvas.getBoundingClientRect();
    pointer = { x: (event.clientX - bounds.left) / bounds.width * 2 - 1, y: (event.clientY - bounds.top) / bounds.height * 2 - 1, active: true };
  });
  canvas.addEventListener('pointerleave', () => { pointer.active = false; });
  new ResizeObserver(resize).observe(art);
  new IntersectionObserver(([entry]) => { inView = entry.isIntersecting; sync(); }).observe(art);
  motion.addEventListener('change', sync);
  phone.addEventListener('change', sync);
  document.addEventListener('visibilitychange', sync);
  new MutationObserver(() => {
    if (!running && inView && !phone.matches && !document.hidden) draw(performance.now(), true);
  }).observe(theme, { attributes: true, attributeFilter: ['data-theme'] });
  resize();
})();
