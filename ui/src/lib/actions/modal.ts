// Accessibility action for modal dialogs: role/aria-modal, an accessible
// name from the first heading, Escape to close, focus trapping and focus
// restoration. Usage:
//
//   <div class="fixed ..." use:modal={{ onClose: () => dispatch('close') }}>
//
// Pass `onClose` in the action options; the component decides what closing
// means (usually dispatching its own `close` event).
const FOCUSABLE =
  'a[href], area[href], input:not([disabled]), select:not([disabled]), ' +
  'textarea:not([disabled]), button:not([disabled]), iframe, object, embed, ' +
  '[contenteditable], [tabindex]:not([tabindex="-1"])';

export function modal(node: HTMLElement, options: { onClose?: () => void } = {}) {
  const previouslyFocused = document.activeElement as HTMLElement | null;

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

  const focusable = () =>
    Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => !el.hasAttribute('disabled') && el.offsetParent !== null
    );

  const onKeydown = (event: KeyboardEvent) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      options.onClose?.();
      return;
    }
    if (event.key !== 'Tab') return;
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

  document.addEventListener('keydown', onKeydown, true);
  requestAnimationFrame(() => {
    const items = focusable();
    (items[0] ?? node).focus();
  });

  return {
    destroy() {
      document.removeEventListener('keydown', onKeydown, true);
      previouslyFocused?.focus?.();
    }
  };
}
