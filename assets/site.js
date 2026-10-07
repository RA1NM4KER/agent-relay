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
