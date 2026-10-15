"use strict";
(() => {
  const demo = document.getElementById("handoff-demo");
  if (!demo) return;
  const bars = [...demo.querySelectorAll(".demo-progress span")];
  const cards = {
    a: demo.querySelector('[data-profile="a"]'),
    b: demo.querySelector('[data-profile="b"]'),
    c: demo.querySelector('[data-profile="c"]'),
  };
  const cardStates = {
    a: document.getElementById("profile-a-state"),
    b: document.getElementById("profile-b-state"),
    c: document.getElementById("profile-c-state"),
  };
  const status = document.getElementById("demo-status");
  const toggle = document.getElementById("demo-toggle");
  const replay = document.getElementById("demo-replay");
  const position = document.getElementById("demo-position");
  const announcement = document.getElementById("demo-announcement");
  const motion = matchMedia("(prefers-reduced-motion: reduce)");

  // Ten narrative beats across three profile cards: claude-main (a), claude-fallback (b),
  // codex-main (c). Durations are tuned so the core "sticky ownership" beat (6) gets the most
  // dwell time — the whole point of this demo — while the transfer itself (4) is quick.
  const durations = [2200, 1800, 2200, 1800, 1400, 2400, 2600, 2200, 1800, 2200];
  const statusLines = [
    "Working on claude-main",
    "Usage limit verified",
    "Continuing on claude-fallback",
    "Fallback exhausted",
    "Transferring context to codex-main",
    "Working on codex-main",
    "claude-main available again",
    "Current owner stays on codex-main",
    "codex-main exhausted",
    "Continuing on claude-main",
  ];
  // Exactly one card is ever the real owner (full border + fill), except step 4's deliberate
  // gap: nothing is "the owner" for the one beat representing the handoff itself in flight.
  const owner = {
    a: [1, 1, 0, 0, 0, 0, 0, 0, 0, 1],
    b: [0, 0, 1, 1, 0, 0, 0, 0, 0, 0],
    c: [0, 0, 0, 0, 0, 1, 1, 1, 1, 0],
  };
  // A card's dot/text tone is independent of ownership: a card can be the structural owner
  // while already fading toward "exhausted" (verified but not yet handed off), and a card can
  // carry the accent tone without being the owner (available again, next eligible, context
  // just transferred).
  const tone = {
    a: ["accent", "muted", "muted", "muted", "muted", "muted", "accent", "accent", "accent", "accent"],
    b: ["muted", "muted", "accent", "muted", "muted", "muted", "muted", "muted", "muted", "muted"],
    c: ["muted", "muted", "muted", "muted", "accent", "accent", "accent", "accent", "muted", "muted"],
  };
  const label = {
    a: ["Active owner", "Exhausted", "Exhausted", "Exhausted", "Exhausted", "Exhausted", "Available again", "Next eligible", "Next eligible", "Active owner"],
    b: ["Standby", "Standby", "Active owner", "Exhausted", "Exhausted", "Exhausted", "Exhausted", "Exhausted", "Exhausted", "Exhausted"],
    c: ["Standby", "Standby", "Standby", "Standby", "Context transferred", "Active owner", "Active owner", "Active owner", "Exhausted", "Exhausted"],
  };
  const lastStep = statusLines.length - 1;

  let step = motion.matches ? lastStep : 0;
  let complete = motion.matches;
  let wantsPlayback = !motion.matches;
  let visible = false;
  let elapsed = 0;
  let previousTime = null;
  let frame = null;

  function progress() {
    bars.forEach((bar, index) => {
      const value =
        complete || index < step
          ? 1
          : index === step
            ? elapsed / durations[step]
            : 0;
      bar.style.setProperty("--progress", value);
    });
  }

  function render() {
    demo.dataset.step = step;
    demo.dataset.complete = complete;
    for (const key of ["a", "b", "c"]) {
      cards[key].dataset.owner = Boolean(owner[key][step]);
      cardStates[key].dataset.tone = tone[key][step];
      cardStates[key].textContent = label[key][step];
    }
    status.textContent = statusLines[step];
    toggle.disabled = complete;
    toggle.textContent = complete
      ? "Complete"
      : motion.matches
        ? "Next step"
        : wantsPlayback
          ? "Pause"
          : "Play";
    toggle.setAttribute(
      "aria-label",
      complete
        ? "Handoff demonstration complete"
        : motion.matches
          ? "Show next handoff step"
          : wantsPlayback
            ? "Pause handoff demonstration"
            : "Play handoff demonstration",
    );
    position.textContent = complete
      ? "Handoff complete"
      : motion.matches
        ? `Step ${step + 1} / ${statusLines.length} · Manual`
        : `Step ${step + 1} / ${statusLines.length}${wantsPlayback ? "" : " · Paused"}`;
    progress();
  }

  function tick(time) {
    if (previousTime !== null) elapsed += time - previousTime;
    previousTime = time;
    if (elapsed >= durations[step]) {
      if (step === lastStep) {
        complete = true;
        wantsPlayback = false;
        render();
        syncPlayback();
        return;
      }
      step += 1;
      elapsed = 0;
      render();
    }
    progress();
    frame = requestAnimationFrame(tick);
  }

  function syncPlayback() {
    cancelAnimationFrame(frame);
    previousTime = null;
    const running =
      wantsPlayback &&
      !complete &&
      !motion.matches &&
      visible &&
      !document.hidden;
    demo.dataset.running = running;
    frame = running ? requestAnimationFrame(tick) : null;
  }

  toggle.addEventListener("click", () => {
    if (complete) return;
    if (motion.matches) {
      step = Math.min(step + 1, lastStep);
      complete = step === lastStep;
      elapsed = 0;
    } else {
      wantsPlayback = !wantsPlayback;
    }
    render();
    syncPlayback();
    announcement.textContent = `${statusLines[step]}. ${motion.matches ? "Manual step." : wantsPlayback ? "Playing." : "Paused."}`;
  });

  replay.addEventListener("click", () => {
    step = 0;
    elapsed = 0;
    complete = false;
    wantsPlayback = !motion.matches;
    render();
    syncPlayback();
    announcement.textContent = motion.matches
      ? "First step shown. Use Next step to continue."
      : "Handoff demonstration restarted.";
  });

  motion.addEventListener("change", () => {
    // A preference change never initiates autoplay. Reduced motion shows the full result.
    wantsPlayback = false;
    if (motion.matches) {
      step = lastStep;
      complete = true;
      elapsed = 0;
    }
    render();
    syncPlayback();
  });
  document.addEventListener("visibilitychange", syncPlayback);
  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      ([entry]) => {
        visible = entry.isIntersecting && entry.intersectionRatio >= 0.25;
        syncPlayback();
      },
      { threshold: 0.25 },
    );
    observer.observe(demo);
  } else {
    visible = true;
  }
  demo.dataset.enhanced = "true";
  demo.querySelector(".demo-playback").hidden = false;
  render();
  syncPlayback();
})();
