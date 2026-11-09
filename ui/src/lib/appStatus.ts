/** Badge classes for a Helm release status (not an app state). */
export function statusClass(status: string): string {
  switch (status) {
    // `helm::release_status` maps a healthy Helm release to `running`; keep
    // `deployed` as a fallback for any caller that passes the raw status.
    case 'running':
    case 'deployed':
      return 'bg-green-900/50 text-green-400';
    case 'failed':
      return 'bg-red-900/50 text-red-400';
    case 'missing':
      return 'bg-amber-900/50 text-amber-400';
    default:
      if (status.startsWith('pending') || status === 'uninstalling') {
        return 'bg-yellow-900/50 text-yellow-300';
      }
      return 'bg-gray-900/50 text-gray-400';
  }
}

export function statusLabel(status: string): string {
  return status || 'unknown';
}
