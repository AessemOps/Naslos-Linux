// noVNC (@novnc/novnc, MPL-2.0) ships no type declarations; this is the
// minimal surface the console uses (CREDITS.md has the provenance).
declare module '@novnc/novnc/lib/rfb.js' {
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, urlOrWs: string | WebSocket, options?: {
      credentials?: { password?: string; username?: string };
      shared?: boolean;
      repeaterID?: string;
    });
    scaleViewport: boolean;
    resizeSession: boolean;
    viewOnly: boolean;
    sendCtrlAltDel(): void;
    sendCredentials(credentials: { password?: string; username?: string }): void;
    focus(options?: { preventScroll?: boolean }): void;
    blur(): void;
    clipboardPasteFrom(text: string): void;
    disconnect(): void;
  }
}
