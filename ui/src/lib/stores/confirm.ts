import { writable } from 'svelte/store';

export type ConfirmTone = 'danger' | 'primary';

/** Options accepted by `confirmAction()`. */
export interface ConfirmOptions {
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Visual/action tone; defaults to `primary`. */
  tone?: ConfirmTone;
  /** When set, the confirm button stays disabled until this exact text is
   * typed. Use it for destructive actions that cannot be undone. */
  typedConfirmation?: string;
}

export interface ConfirmRequest extends ConfirmOptions {
  id: number;
  confirmLabel: string;
  cancelLabel: string;
  tone: ConfirmTone;
}

/** The request currently rendered by ConfirmDialog, or null when closed. */
export const confirmState = writable<ConfirmRequest | null>(null);

interface ActiveConfirm {
  request: ConfirmRequest;
  resolve: (value: boolean) => void;
}

let active: ActiveConfirm | null = null;
let counter = 0;

/**
 * Ask the user to confirm an action and resolve with their answer. Replaces
 * the browser `confirm()`; later pages mount `<ConfirmDialog />` once in the
 * layout and call this from event handlers:
 *
 *   if (!(await confirmAction({ title: 'Delete pool', message: '…', tone: 'danger' }))) return;
 *
 * A second request while one is open rejects the first with `false` (the
 * first caller must not hang forever) and shows the new request.
 */
export function confirmAction(options: ConfirmOptions): Promise<boolean> {
  if (active) resolveActive(false);
  const request: ConfirmRequest = {
    ...options,
    id: ++counter,
    confirmLabel: options.confirmLabel || 'Confirm',
    cancelLabel: options.cancelLabel || 'Cancel',
    tone: options.tone || 'primary'
  };
  return new Promise<boolean>((resolve) => {
    active = { request, resolve };
    confirmState.set(request);
  });
}

/** Close the open confirmation and resolve its promise. Safe to call when no
 * dialog is open (the component's close paths can overlap via Escape and a
 * button click). */
export function resolveConfirm(result: boolean) {
  resolveActive(result);
}

function resolveActive(result: boolean) {
  const current = active;
  active = null;
  confirmState.set(null);
  current?.resolve(result);
}
