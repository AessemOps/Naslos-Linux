import { writable } from 'svelte/store';

export type Theme = 'light' | 'dark';

const KEY = 'naslos:theme';

function initial(): Theme {
  if (typeof document !== 'undefined' && document.documentElement.dataset.theme === 'light') {
    return 'light';
  }
  return 'dark';
}

/** The active theme. The pre-paint script in app.html sets data-theme before
 * hydration, so this starts in sync with what the user is already looking at. */
export const theme = writable<Theme>(initial());

theme.subscribe((value) => {
  if (typeof document === 'undefined') return;
  document.documentElement.dataset.theme = value;
  try {
    localStorage.setItem(KEY, value);
  } catch {
    // Private-browsing storage failures just lose the preference.
  }
});

export function toggleTheme() {
  theme.update((t) => (t === 'light' ? 'dark' : 'light'));
}
