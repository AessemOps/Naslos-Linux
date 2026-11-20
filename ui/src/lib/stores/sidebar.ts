import { writable } from 'svelte/store';

const KEY = 'naslos:sidebar';

function initial(): boolean {
  try {
    return localStorage.getItem(KEY) === 'collapsed';
  } catch {
    return false;
  }
}

/** Sidebar collapsed state, persisted across sessions. Read by both
 * Sidebar.svelte (width) and +layout.svelte (content margin). */
export const sidebarCollapsed = writable<boolean>(initial());

export function toggleSidebar() {
  sidebarCollapsed.update((c) => {
    const next = !c;
    try {
      localStorage.setItem(KEY, next ? 'collapsed' : 'expanded');
    } catch {
      // Ignore: the toggle still works for this session.
    }
    return next;
  });
}
