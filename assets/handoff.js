"use strict";
(() => {
  const demo = document.getElementById("handoff-demo");
  if (!demo) return;
  const steps = [...demo.querySelectorAll(".demo-steps li")];
  const bars = [...demo.querySelectorAll(".demo-progress span")];
  const source = demo.querySelector('[data-profile="source"]');
  const target = demo.querySelector('[data-profile="target"]');
  const sourceState = document.getElementById("source-state");
  const targetState = document.getElementById("target-state");
  const toggle = document.getElementById("demo-toggle");
  const replay = document.getElementById("demo-replay");
  const position = document.getElementById("demo-position");
  const announcement = document.getElementById("demo-announcement");
  const motion = matchMedia("(prefers-reduced-motion: reduce)");
  const durations = [3600, 3000, 3800, 5000];
  const sourceStates = ["Active owner", "Exhausted", "Stopped", "Stopped"];
  const targetStates = ["Standby", "Standby", "Eligible", "Active owner"];
  let step = motion.matches ? 3 : 0;
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
    steps.forEach((item, index) => {
      item.dataset.complete = index < step || complete;
      if (index === step && !complete)
        item.setAttribute("aria-current", "step");
      else item.removeAttribute("aria-current");
    });
    // Never show two active owners, including the source-stop interval.
    source.dataset.owner = step < 2;
    target.dataset.owner = step === 3;
    sourceState.textContent = sourceStates[step];
    targetState.textContent = targetStates[step];
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
        ? `Step ${step + 1} / 4 · Manual`
        : `Step ${step + 1} / 4${wantsPlayback ? "" : " · Paused"}`;
    progress();
  }

  function tick(time) {
    if (previousTime !== null) elapsed += time - previousTime;
    previousTime = time;
    if (elapsed >= durations[step]) {
      if (step === steps.length - 1) {
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
      step = Math.min(step + 1, steps.length - 1);
      complete = step === steps.length - 1;
      elapsed = 0;
    } else {
      wantsPlayback = !wantsPlayback;
    }
    render();
    syncPlayback();
    announcement.textContent = `${steps[step].querySelector("h3").textContent}. ${motion.matches ? "Manual step." : wantsPlayback ? "Playing." : "Paused."}`;
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
      step = 3;
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
