import { writable } from 'svelte/store';

const KEY = 'naslos:sidebar';

function initial(): boolean {
  try {
    return localStorage.getItem(KEY) === 'collapsed';
  } catch {
    return false;
  }
}

/** Desktop sidebar collapsed state, persisted across sessions. The mobile
 * drawer ignores it and always shows labels. Read by both Sidebar.svelte
 * (width) and +layout.svelte (content offset). */
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

/** Off-canvas navigation state below the lg breakpoint. */
export const mobileNavOpen = writable<boolean>(false);

export function openMobileNav() {
  mobileNavOpen.set(true);
}

export function closeMobileNav() {
  mobileNavOpen.set(false);
}

export function toggleMobileNav() {
  mobileNavOpen.update((open) => !open);
}
