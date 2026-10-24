"use strict";
document.documentElement.classList.add("js");
const themeButton = document.querySelector(".theme-button");
function syncThemeLabel() {
  const next =
    document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  themeButton.setAttribute("aria-label", `Switch to ${next} theme`);
  themeButton.title = `Switch to ${next} theme`;
}
themeButton.hidden = false;
syncThemeLabel();
themeButton.addEventListener("click", () => {
  const theme =
    document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem("relay-theme", theme);
  } catch (_) {
    /* Optional persistence. */
  }
  syncThemeLabel();
});
for (const button of document.querySelectorAll(".copy-button")) {
  button.addEventListener("click", async () => {
    const code = button.parentElement.querySelector("code");
    const status = document.getElementById("copy-status");
    button.disabled = true;
    try {
      await navigator.clipboard.writeText(code.textContent);
      button.textContent = "Copied";
      status.textContent = "Command copied to clipboard.";
    } catch (_) {
      const range = document.createRange();
      range.selectNodeContents(code);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      button.textContent = "Select";
      status.textContent =
        "Clipboard unavailable. Command selected; copy it with your keyboard.";
    }
    setTimeout(() => {
      button.textContent = "Copy";
      button.disabled = false;
    }, 1800);
  });
}
const search = document.getElementById("docs-search");
if (search) {
  const results = document.getElementById("search-results");
  const status = document.getElementById("search-status");
  let indexPromise;
  let requestId = 0;
  function loadIndex() {
    if (!indexPromise)
      indexPromise = fetch("../assets/search-index.json")
        .then((response) => {
          if (!response.ok) throw new Error("Search index unavailable");
          return response.json();
        })
        .catch((error) => {
          indexPromise = undefined;
          throw error;
        });
    return indexPromise;
  }
  function closeSearch() {
    ++requestId;
    results.hidden = true;
    status.textContent = "";
  }
  search.addEventListener("input", async () => {
    const id = ++requestId;
    const query = search.value.trim().toLowerCase();
    results.replaceChildren();
    results.hidden = true;
    if (!query) {
      status.textContent = "";
      return;
    }
    status.textContent = "Searching…";
    try {
      const index = await loadIndex();
      if (id !== requestId) return;
      const terms = query.split(/\s+/);
      const matches = index
        .map((entry) => {
          const haystack =
            `${entry.title} ${entry.page || ""} ${entry.text}`.toLowerCase();
          return {
            entry,
            score: terms.every((term) => haystack.includes(term))
              ? terms.reduce(
                  (score, term) =>
                    score + (entry.title.toLowerCase().includes(term) ? 5 : 1),
                  0,
                )
              : 0,
          };
        })
        .filter((item) => item.score)
        .sort((a, b) => b.score - a.score)
        .slice(0, 8);
      status.textContent = matches.length
        ? `${matches.length} results. Tab to browse.`
        : "No results. Try “resume”, “trust”, or “switch”.";
      for (const { entry } of matches) {
        const link = document.createElement("a");
        link.href = entry.url;
        const context = document.createElement("small");
        context.textContent = entry.page || "Documentation";
        const title = document.createElement("strong");
        title.textContent = entry.title;
        const snippet = document.createElement("p");
        snippet.textContent =
          entry.text.slice(0, 145) + (entry.text.length > 145 ? "…" : "");
        link.append(context, title, snippet);
        results.append(link);
      }
      results.hidden = !matches.length;
    } catch (_) {
      if (id === requestId)
        status.textContent =
          "Search is unavailable. Use the documentation navigation or your browser’s Find command.";
    }
  });
  document.addEventListener("keydown", (event) => {
    const editing =
      /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName) ||
      document.activeElement.isContentEditable;
    if (
      event.key === "/" &&
      !editing &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey
    ) {
      event.preventDefault();
      search.focus();
    }
    if (event.key === "Escape") closeSearch();
  });
  results.addEventListener("click", (event) => {
    if (event.target.closest("a")) closeSearch();
  });
  document.addEventListener("click", (event) => {
    if (!event.target.closest(".search-area")) closeSearch();
  });
}


// Keep the complete status excerpt as the accessible, no-JS fallback.
const terminal = document.querySelector(".hero-terminal .terminal");
if (terminal) {
  const original = terminal.querySelector(".cli-output");
  const playback = terminal.querySelector(".terminal-playback");
  const label = terminal.querySelector(".terminal-demo-label");
  const motion = matchMedia("(prefers-reduced-motion: reduce)");
  const frames = [
    { command: "relay status --live", output: original.querySelector("code").innerHTML.split("</span>\n").slice(1).join("</span>\n").replace(/^\n/, "") },
    { command: "relay doctor", output: `<span class="cli-heading">Agent Relay Doctor</span>

<span class="cli-heading">Current project</span>
  /repos/my-project

<span class="cli-heading">Automatic handoff</span>
  <span class="cli-success">✓ Ready for automatic handoff.</span>

<span class="cli-heading">Passed checks</span>
  <span class="cli-success">✓ claude-main authenticated</span>
  <span class="cli-success">✓ claude-backup authenticated</span>
  <span class="cli-success">✓ codex-main authenticated</span>` },
    { command: "relay profile list", output: `<span class="cli-heading">claude-main · Claude</span>
  Directory: /profiles/claude-main

<span class="cli-heading">claude-backup · Claude</span>
  Directory: /profiles/claude-backup

<span class="cli-heading">codex-main · Codex</span>
  Directory: /profiles/codex-main` },
  ];
  const screen = original.cloneNode(false);
  screen.classList.add("terminal-screen");
  screen.removeAttribute("tabindex");
  screen.setAttribute("aria-hidden", "true");
  screen.hidden = true;
  original.after(screen);
  let paused = false;
  let visible = true;
  let timer;
  let frame = 0;
  let character = 0;
  let line = 0;
  let phase = "start";
  let typed;
  let output;
  function running() {
    return !paused && !motion.matches && visible && !document.hidden;
  }
  function schedule(delay) {
    clearTimeout(timer);
    if (running()) timer = setTimeout(tick, delay);
  }
  function tick() {
    const current = frames[frame];
    if (phase === "start") {
      screen.innerHTML = '<code><span class="cli-command"><span>$ </span><span class="terminal-typed"></span><span class="terminal-cursor">▍</span></span><span class="terminal-result"></span></code>';
      typed = screen.querySelector(".terminal-typed");
      output = screen.querySelector(".terminal-result");
      character = 0;
      line = 0;
      phase = "type";
      schedule(600);
    } else if (phase === "type") {
      typed.textContent = current.command.slice(0, ++character);
      if (character === current.command.length) phase = "output";
      schedule(phase === "output" ? 550 : 45 + (character % 4) * 18);
    } else if (phase === "output") {
      screen.querySelector(".terminal-cursor").hidden = true;
      const lines = current.output.split("\n");
      output.innerHTML = "\n\n" + lines.slice(0, ++line).join("\n");
      if (line === lines.length) phase = "hold";
      schedule(phase === "hold" ? 6000 : 85);
    } else {
      frame = (frame + 1) % frames.length;
      phase = "start";
      tick();
    }
  }
  function sync() {
    clearTimeout(timer);
    const animate = !motion.matches;
    original.classList.toggle("terminal-transcript", animate);
    screen.hidden = !animate;
    playback.hidden = !animate;
    label.hidden = animate;
    playback.textContent = paused ? "Play demo" : "Pause demo";
    terminal.classList.toggle("terminal-paused", !running());
    schedule(250);
  }
  playback.addEventListener("click", () => { paused = !paused; sync(); });
  motion.addEventListener("change", sync);
  document.addEventListener("visibilitychange", sync);
  new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; sync(); }).observe(terminal);
  sync();
}
