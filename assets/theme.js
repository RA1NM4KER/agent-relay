try {
  const savedTheme = localStorage.getItem("relay-theme");
  if (savedTheme === "light" || savedTheme === "dark")
    document.documentElement.dataset.theme = savedTheme;
} catch (_) {
  /* Storage can be unavailable in private browsing. */
}
