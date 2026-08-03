export const systemLight = window.matchMedia("(prefers-color-scheme: light)");

export function applyTheme(theme) {
  const resolved =
    theme === "light"
      ? "light"
      : theme === "dark"
        ? "dark"
        : systemLight.matches
          ? "light"
          : "dark";
  document.documentElement.dataset.theme = resolved;
}
