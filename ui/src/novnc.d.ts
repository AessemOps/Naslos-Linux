// noVNC (@novnc/novnc, MPL-2.0) ships no type declarations; this is the
// minimal surface the console uses (CREDITS.md has the provenance).
declare module '@novnc/novnc/lib/rfb.js' {
  // Minimal used surface (CREDITS.md has the provenance).
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, urlOrWs: string | WebSocket, options?: {
      credentials?: { password?: string; username?: string };
      shared?: boolean;
    });
    scaleViewport: boolean;
    resizeSession: boolean;
    sendCtrlAltDel(): void;
    disconnect(): void;
  }
}
