// Focus trap for the off-canvas mobile navigation. The drawer is always in the
// DOM (it slides out of view on small screens), so this action only activates
// while `open` is true: it traps Tab, locks body scroll and focuses the first
// link. Escape is handled by the Sidebar's window keydown listener.
import { lockBodyScroll, unlockBodyScroll } from './modal';

const FOCUSABLE =
  'a[href], area[href], input:not([disabled]), select:not([disabled]), ' +
  'textarea:not([disabled]), button:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function mobileDrawer(node: HTMLElement, options: { open: boolean }) {
  let open = options.open;
  let locked = false;
  let previouslyFocused: HTMLElement | null = null;

  const focusable = () =>
    Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => !el.hasAttribute('disabled') && el.offsetParent !== null
    );

  const onKeydown = (event: KeyboardEvent) => {
    if (!open || event.key !== 'Tab') return;
    const items = focusable();
    if (items.length === 0) {
      event.preventDefault();
      node.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    if (event.shiftKey && (document.activeElement === first || document.activeElement === node)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  function activate() {
    if (locked) return;
    locked = true;
    previouslyFocused = document.activeElement as HTMLElement | null;
    lockBodyScroll();
    document.addEventListener('keydown', onKeydown, true);
    requestAnimationFrame(() => {
      if (open) (focusable()[0] ?? node).focus();
    });
  }

  function deactivate(restoreFocus = true) {
    if (!locked) return;
    locked = false;
    document.removeEventListener('keydown', onKeydown, true);
    unlockBodyScroll();
    // Return focus to the hamburger when it is still visible. On a resize to
    // desktop the trigger is hidden, so leaving focus where the browser put it
    // is better than focusing a display:none control.
    if (restoreFocus && previouslyFocused && previouslyFocused.offsetParent !== null) {
      previouslyFocused.focus();
    }
    previouslyFocused = null;
  }

  if (open) activate();

  return {
    update(next: { open: boolean }) {
      if (next.open === open) return;
      open = next.open;
      if (open) activate();
      else deactivate();
    },
    destroy() {
      deactivate(false);
    }
  };
}
