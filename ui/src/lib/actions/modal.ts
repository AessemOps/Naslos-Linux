// Accessibility action for modal dialogs: role/aria-modal, an accessible name
// from the first heading, Escape to close, focus trapping, focus restoration,
// body scroll locking and `inert` on the background.
//
//   <div class="fixed ..." use:modal={{ onClose: () => dispatch('close') }}>
//
// Pass `onClose` in the action options; the component decides what closing
// means (usually dispatching its own `close` event). Only the top-most modal
// handles Escape and Tab, so stacked dialogs close one at a time.
const FOCUSABLE =
  'a[href], area[href], input:not([disabled]), select:not([disabled]), ' +
  'textarea:not([disabled]), button:not([disabled]), iframe, object, embed, ' +
  '[contenteditable], [tabindex]:not([tabindex="-1"])';

interface ModalOptions {
  onClose?: () => void;
}

interface ModalInstance {
  node: HTMLElement;
  onClose: () => void;
  previouslyFocused: HTMLElement | null;
  inerted: { el: HTMLElement; wasInert: boolean }[];
}

const stack: ModalInstance[] = [];

let scrollLocks = 0;
let previousBodyOverflow = '';
let previousBodyPaddingRight = '';

/** Prevent the page behind one or more overlays from scrolling. Counter-based
 * so closing a nested modal does not unlock the page under the outer one. */
export function lockBodyScroll() {
  if (typeof document === 'undefined') return;
  if (scrollLocks === 0) {
    previousBodyOverflow = document.body.style.overflow;
    previousBodyPaddingRight = document.body.style.paddingRight;
    const scrollbar = window.innerWidth - document.documentElement.clientWidth;
    document.body.style.overflow = 'hidden';
    // Avoid a layout shift when the scrollbar disappears on desktop.
    if (scrollbar > 0) document.body.style.paddingRight = `${scrollbar}px`;
  }
  scrollLocks += 1;
}

export function unlockBodyScroll() {
  if (typeof document === 'undefined') return;
  scrollLocks = Math.max(0, scrollLocks - 1);
  if (scrollLocks === 0) {
    document.body.style.overflow = previousBodyOverflow;
    document.body.style.paddingRight = previousBodyPaddingRight;
  }
}

// Mark every element on the path from <body> down to the dialog as the active
// branch, and inert all its siblings. This leaves the dialog (and only the
// dialog) interactive without needing a portal.
function inertBackground(instance: ModalInstance) {
  let child: HTMLElement = instance.node;
  while (child.parentElement) {
    const parent: HTMLElement = child.parentElement;
    for (const sibling of Array.from(parent.children)) {
      if (sibling === child || !(sibling instanceof HTMLElement)) continue;
      if (sibling.hasAttribute('inert')) continue;
      sibling.setAttribute('inert', '');
      instance.inerted.push({ el: sibling, wasInert: false });
    }
    if (parent === document.body) break;
    child = parent;
  }
}

function restoreBackground(instance: ModalInstance) {
  for (const { el, wasInert } of instance.inerted) {
    if (!wasInert) el.removeAttribute('inert');
  }
  instance.inerted = [];
}

function focusable(node: HTMLElement): HTMLElement[] {
  return Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => !el.hasAttribute('disabled') && el.offsetParent !== null
  );
}

export function modal(node: HTMLElement, options: ModalOptions = {}) {
  const instance: ModalInstance = {
    node,
    onClose: options.onClose ?? (() => {}),
    previouslyFocused: document.activeElement as HTMLElement | null,
    inerted: []
  };

  node.setAttribute('role', 'dialog');
  node.setAttribute('aria-modal', 'true');
  if (!node.hasAttribute('tabindex')) node.setAttribute('tabindex', '-1');
  if (!node.getAttribute('aria-labelledby') && !node.getAttribute('aria-label')) {
    const heading = node.querySelector<HTMLElement>('h1, h2, h3');
    if (heading) {
      if (!heading.id) {
        heading.id = `modal-title-${Math.random().toString(36).slice(2, 9)}`;
      }
      node.setAttribute('aria-labelledby', heading.id);
    }
  }

  const onKeydown = (event: KeyboardEvent) => {
    // Only the top-most dialog reacts: a nested modal owns Escape first.
    if (stack[stack.length - 1] !== instance) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      instance.onClose?.();
      return;
    }
    if (event.key !== 'Tab') return;
    const items = focusable(node);
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

  stack.push(instance);
  lockBodyScroll();
  inertBackground(instance);
  document.addEventListener('keydown', onKeydown, true);
  requestAnimationFrame(() => {
    if (stack[stack.length - 1] !== instance) return;
    const items = focusable(node);
    (items[0] ?? node).focus();
  });

  return {
    update(next: ModalOptions = {}) {
      instance.onClose = next.onClose ?? (() => {});
    },
    destroy() {
      const index = stack.indexOf(instance);
      if (index !== -1) stack.splice(index, 1);
      document.removeEventListener('keydown', onKeydown, true);
      restoreBackground(instance);
      unlockBodyScroll();
      // Restore focus only when this was the last dialog, or when focus was
      // inside this dialog and the remaining top dialog can take over.
      const top = stack[stack.length - 1];
      if (!top) {
        instance.previouslyFocused?.focus?.();
      } else if (node.contains(document.activeElement)) {
        (focusable(top.node)[0] ?? top.node).focus();
      }
    }
  };
}
