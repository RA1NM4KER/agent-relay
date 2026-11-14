"use strict";
// Homepage art and entrances are optional; every section is visible without JS.
if (document.body.classList.contains('relay-home')) {
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  const hero = document.querySelector('.front-hero');
  const sculpture = document.querySelector('.relay-sculpture');
  motion.addEventListener('change', () => {
    sculpture.style.removeProperty('--art-x');
    sculpture.style.removeProperty('--art-y');
  });
  if (matchMedia('(hover: hover) and (pointer: fine)').matches) {
    let pendingFrame;
    hero.addEventListener('pointermove', (event) => {
      if (motion.matches) return;
      const bounds = hero.getBoundingClientRect();
      const x = (event.clientX - bounds.left) / bounds.width - .5;
      const y = (event.clientY - bounds.top) / bounds.height - .5;
      cancelAnimationFrame(pendingFrame);
      pendingFrame = requestAnimationFrame(() => {
        sculpture.style.setProperty('--art-y', `${x * 10}deg`);
        sculpture.style.setProperty('--art-x', `${-y * 7}deg`);
      });
    });
    hero.addEventListener('pointerleave', () => {
      cancelAnimationFrame(pendingFrame);
      sculpture.style.removeProperty('--art-x');
      sculpture.style.removeProperty('--art-y');
    });
  }
  const entrances = document.querySelectorAll('.front-preview, .front-section-intro, .front-steps li, .front-boundary-intro, .front-principles > div, .front-questions, .front-start');
  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        entry.target.classList.remove('reveal-pending');
        observer.unobserve(entry.target);
      }
    }
  }, { threshold: .08 });
  for (const element of entrances) {
    element.classList.add('reveal-ready', 'reveal-pending');
    observer.observe(element);
  }
}

// One output element serves playback, assistive technology, and the static fallback.
(() => {
  const terminal = document.querySelector('.front-console');
  if (!terminal) return;
  const original = terminal.querySelector('.cli-output');
  const motion = matchMedia('(prefers-reduced-motion: reduce)');
  const screen = original;
  const fallback = original.innerHTML;
  const frames = [
    { command: 'relay status --live', output: '<span class="front-output-title">Current</span>\n  <span class="cli-profile">claude-main</span> · Claude\n  Session 9cc4d728\n\n<span class="front-output-title">Configured fallback chain</span>\n  1. <span class="cli-profile">claude-backup</span>\n  2. <span class="cli-profile">codex-main</span>' },
    { command: 'relay doctor', output: '<span class="front-output-title">Agent Relay Doctor</span>\n\n<span class="front-output-title">Automatic handoff</span>\n  <span class="cli-success">✓ Ready for automatic handoff.</span>\n\n<span class="front-output-title">Passed checks</span>\n  <span class="cli-success">✓ claude-main authenticated</span>\n  <span class="cli-success">✓ claude-backup authenticated</span>\n  <span class="cli-success">✓ codex-main authenticated</span>' },
    { command: 'relay profile list', output: '<span class="cli-profile">claude-main</span> · Claude\n  Directory: <span class="cli-path">/profiles/claude-main</span>\n\n<span class="cli-profile">claude-backup</span> · Claude\n  Directory: <span class="cli-path">/profiles/claude-backup</span>\n\n<span class="cli-profile">codex-main</span> · Codex\n  Directory: <span class="cli-path">/profiles/codex-main</span>' },
  ];
  let frame = 0;
  let timer;
  let visible = false;
  let phase = 'typing';
  let character = 0;
  let line = 0;
  let delay = 500;
  let command;
  let output;
  let cursor;
  function paintCommand(text) {
    let word = 0;
    const parts = text.split(/(\s+)/).map(part => {
      if (/^\s+$/.test(part)) return document.createTextNode(part);
      const token = document.createElement('span');
      token.className = word++ === 0 ? 'cli-executable' : part.startsWith('--') ? 'cli-flag' : 'cli-argument';
      token.textContent = part;
      return token;
    });
    command.replaceChildren(...parts);
  }
  function render() {
    const current = frames[frame];
    screen.innerHTML = '<code><span class="terminal-command-line"><span class="front-prompt">$ </span><span class="front-command"></span><span class="terminal-cursor" aria-hidden="true">▍</span></span><span class="terminal-response"></span></code>';
    command = screen.querySelector('.front-command');
    output = screen.querySelector('.terminal-response');
    cursor = screen.querySelector('.terminal-cursor');
    character = 0;
    line = 0;
    phase = 'typing';
    delay = 400;
    terminal.dataset.command = current.command;
    terminal.dataset.phase = phase;
  }
  function tick() {
    const current = frames[frame];
    if (phase === 'typing') {
      paintCommand(current.command.slice(0, ++character));
      if (character === current.command.length) {
        phase = 'response';
        delay = 380;
      } else {
        delay = 45 + (character % 3) * 14;
      }
    } else if (phase === 'response') {
      cursor.hidden = true;
      if (line === 0) output.append(document.createElement('br'));
      const lines = current.output.split('\n');
      const row = document.createElement('span');
      row.className = 'terminal-line';
      row.innerHTML = lines[line++] || ' ';
      output.append(row);
      if (line === lines.length) {
        phase = 'holding';
        delay = 5200;
      } else delay = 110;
    } else {
      frame = (frame + 1) % frames.length;
      render();
    }
    terminal.dataset.phase = phase;
    schedule();
  }
  function schedule() {
    clearTimeout(timer);
    if (!motion.matches && visible && !document.hidden) timer = setTimeout(tick, delay);
  }
  function sync() {
    const animate = !motion.matches;
    screen.classList.toggle('terminal-screen', animate);
    if (animate) render();
    else {
      screen.innerHTML = fallback;
      terminal.dataset.command = 'relay status --live';
      terminal.dataset.phase = 'static';
      frame = 0;
    }
    schedule();
  }
  motion.addEventListener('change', sync);
  document.addEventListener('visibilitychange', schedule);
  new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; schedule(); }, { threshold: .15 }).observe(terminal);
  render();
  sync();
})();
