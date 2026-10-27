/** Apply the persisted application theme to the document root. */
export function applyTheme(theme: string): void {
  const root = document.documentElement;
  const light = theme === 'light';

  if (light) {
    root.setAttribute('data-mode', 'light');
  } else {
    root.removeAttribute('data-mode');
  }

  root.style.colorScheme = light ? 'light' : 'dark';
}
