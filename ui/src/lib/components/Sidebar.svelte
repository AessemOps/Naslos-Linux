<script lang="ts">
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { appUpdates, startAppUpdates } from '$lib/stores/appUpdates';
  import { sidebarCollapsed, toggleSidebar } from '$lib/stores/sidebar';
  import Icon from './Icon.svelte';
  import ThemeToggle from './ThemeToggle.svelte';

  interface NavItem {
    name: string;
    path: string;
    icon: string;
  }

  interface NavGroup {
    label: string;
    items: NavItem[];
  }

  // Grouped navigation. Group membership is a proposal from the UI plan; the
  // items themselves are exactly the pre-overhaul set of routes.
  const navGroups: NavGroup[] = [
    {
      label: 'Overview',
      items: [
        { name: 'Dashboard', path: '/', icon: 'dashboard' },
        { name: 'Notifications', path: '/notifications', icon: 'bell' }
      ]
    },
    {
      label: 'Storage',
      items: [
        { name: 'Pools', path: '/pools', icon: 'database' },
        { name: 'Datasets', path: '/datasets', icon: 'folder-tree' },
        { name: 'Disks', path: '/disks', icon: 'hard-drive' },
        { name: 'Disk Health', path: '/disks/health', icon: 'heart-pulse' }
      ]
    },
    {
      label: 'Access',
      items: [
        { name: 'Users', path: '/users', icon: 'user' },
        { name: 'Groups', path: '/groups', icon: 'users' },
        { name: 'Shares', path: '/shares', icon: 'link' }
      ]
    },
    {
      label: 'Workloads',
      items: [
        { name: 'Apps', path: '/apps', icon: 'package' },
        { name: 'Virtual Machines', path: '/vms', icon: 'monitor' }
      ]
    },
    {
      label: 'Networking',
      items: [
        { name: 'Domains', path: '/domains', icon: 'globe' },
        { name: 'Dynamic DNS', path: '/dns', icon: 'earth' }
      ]
    },
    {
      label: 'System',
      items: [
        { name: 'Backups', path: '/backups', icon: 'cloud' },
        { name: 'Terminal', path: '/terminal', icon: 'terminal' },
        { name: 'Settings', path: '/settings', icon: 'settings' }
      ]
    }
  ];

  const navItems: NavItem[] = navGroups.flatMap((g) => g.items);

  // Longest-prefix match so detail routes highlight their parent: /pools/tank
  // keeps Pools lit, /settings/network keeps Settings lit, and /disks/health
  // wins over /disks because its path is longer.
  function activePath(current: string): string {
    let best = '';
    for (const item of navItems) {
      const match =
        item.path === '/' ? current === '/' : current === item.path || current.startsWith(`${item.path}/`);
      if (match && item.path.length > best.length) best = item.path;
    }
    return best;
  }

  // Administrative views are operator-only. The API reports the proxy-injected
  // groups on /api/auth/me; when it cannot be read (anonymous, or the dev
  // posture with auth off) the extra link simply stays hidden.
  let isAdmin = false;
  let username = '';
  let updateAvailable = false;

  onMount(async () => {    try {
      const res = await fetch('/api/auth/me');
      if (!res.ok) return;
      const data = await res.json();
      username = data.username ?? '';
      const groups = String(data.groups ?? '')
        .split(',')
        .map((g) => g.trim());
      isAdmin = groups.includes('naslos_admins');
    } catch {
      // Hidden when the identity cannot be read.
    }
    try {
      const res = await fetch('/api/system/updates');
      if (res.ok) {
        const updates = await res.json();
        updateAvailable = Boolean(updates?.available && updates?.stepping?.allowed);
      }
    } catch {
      // No update badge when the API cannot answer (e.g. during a reboot).
    }
  });

  // The update summary is shared with the Apps page (Installed tab badge) and
  // polls every 60s while the document is visible.
  onMount(() => {
    const stop = startAppUpdates();
    return stop;
  });

  // The session belongs to Authelia's forwardAuth portal, so signing out is a
  // POST to its logout endpoint (same origin, routed straight to the portal).
  // The redirect then lands on the login portal; if the session was already
  // dead the POST errors and the redirect is still correct.
  let loggingOut = false;

  async function logout() {
    if (loggingOut) return;
    loggingOut = true;
    try {
      await fetch('/authelia/api/logout', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ targetURL: '/' })
      });
    } catch {
      // Ignore: the redirect below still reaches the login portal.
    }
    window.location.assign('/');
  }
</script>

<aside
  class="fixed left-0 top-0 h-screen bg-naslos-surface border-r border-naslos-border flex flex-col z-40 transition-[width] duration-200 {$sidebarCollapsed
    ? 'w-16'
    : 'w-64'}"
>
  <div class="p-4 border-b border-naslos-border flex items-center gap-3 {$sidebarCollapsed ? 'justify-center' : ''}">
    <img src="/logo.png" alt="Naslos logo" class="w-10 h-10 shrink-0" />
    <div class="min-w-0">
      <!-- Kept in the DOM (sr-only) when collapsed so the brand heading is
           always exposed to assistive tech. -->
      <h1 class="text-2xl font-bold text-naslos-link {$sidebarCollapsed ? 'sr-only' : ''}">Naslos</h1>
      <p class="text-xs text-gray-500 mt-1 {$sidebarCollapsed ? 'sr-only' : ''}">Talos Linux NAS</p>
    </div>
    {#if !$sidebarCollapsed}
      <button
        type="button"
        class="icon-btn ml-auto text-gray-400 hover:text-naslos-strong hover:bg-naslos-border"
        aria-expanded="false"
        aria-controls="sidebar-nav"
        aria-label="Collapse the sidebar"
        title="Collapse the sidebar"
        on:click={toggleSidebar}
      >
        <Icon name="chevrons-left" />
      </button>
    {/if}
  </div>

  {#if $sidebarCollapsed}
    <div class="p-2 border-b border-naslos-border flex justify-center">
      <button
        type="button"
        class="icon-btn text-gray-400 hover:text-naslos-strong hover:bg-naslos-border"
        aria-expanded="true"
        aria-controls="sidebar-nav"
        aria-label="Expand the sidebar"
        title="Expand the sidebar"
        on:click={toggleSidebar}
      >
        <Icon name="chevron-right" />
      </button>
    </div>
  {/if}

  <nav id="sidebar-nav" class="flex-1 overflow-y-auto p-2 space-y-4" aria-label="Main navigation">
    {#each navGroups as group}
      <div>
        <div class="px-3 pb-1 text-[11px] font-semibold uppercase tracking-wider text-gray-500 {$sidebarCollapsed ? 'sr-only' : ''}">
          {group.label}
        </div>
        <div class="space-y-0.5">
          {#each group.items as item}
            {@const active = activePath(page.url.pathname) === item.path}
            <a
              href={item.path}
              class="flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors {$sidebarCollapsed
                ? 'justify-center'
                : ''} {active
                ? 'bg-naslos-primary text-white'
                : 'text-gray-400 hover:bg-naslos-border hover:text-naslos-strong'}"
              aria-current={active ? 'page' : undefined}
              title={$sidebarCollapsed ? item.name : undefined}
            >
              <Icon name={item.icon} />
              <span class="font-medium {$sidebarCollapsed ? 'sr-only' : ''}">{item.name}</span>
              {#if item.path === '/settings' && updateAvailable}
                <span
                  class="ml-auto w-2 h-2 rounded-full bg-naslos-accent shrink-0"
                  role="status"
                  aria-label="A system update is available"
                  title="A system update is available"
                ></span>
              {:else if item.path === '/apps' && $appUpdates.count > 0}
                <span
                  class="ml-auto w-2 h-2 rounded-full bg-naslos-accent shrink-0"
                  role="status"
                  aria-label={`${$appUpdates.count} app update${$appUpdates.count === 1 ? '' : 's'} available`}
                  title={`${$appUpdates.count} app update${$appUpdates.count === 1 ? '' : 's'} available`}
                ></span>
              {/if}
            </a>
          {/each}
        </div>
      </div>
    {/each}

    {#if isAdmin}
      <a
        href="/traefik/dashboard/"
        target="_blank"
        rel="noreferrer"
        class="flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors text-gray-400 hover:bg-naslos-border hover:text-naslos-strong {$sidebarCollapsed
          ? 'justify-center'
          : ''}"
        title={$sidebarCollapsed ? 'Traefik' : undefined}
      >
        <Icon name="compass" />
        <span class="font-medium {$sidebarCollapsed ? 'sr-only' : ''}">Traefik</span>
        {#if !$sidebarCollapsed}
          <span class="ml-auto"><Icon name="external-link" size={14} /></span>
        {/if}
      </a>
    {/if}
  </nav>

  <div class="p-2 border-t border-naslos-border space-y-1">
    <button
      type="button"
      class="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors text-gray-400 hover:bg-naslos-border hover:text-naslos-strong disabled:opacity-50 disabled:cursor-not-allowed {$sidebarCollapsed
        ? 'justify-center'
        : ''}"
      on:click={logout}
      disabled={loggingOut}
    >
      <Icon name="log-out" />
      {#if loggingOut}
        <span class="font-medium {$sidebarCollapsed ? 'sr-only' : ''}">Signing out…</span>
      {:else}
        <span class="font-medium {$sidebarCollapsed ? 'sr-only' : ''}">Sign out</span>
      {/if}
    </button>
    <div class="flex items-center gap-1 px-1 {$sidebarCollapsed ? 'justify-center' : 'justify-between'}">
      <ThemeToggle />
      {#if !$sidebarCollapsed}
        <p class="text-xs text-gray-500 truncate">
          {#if username}{username} · {/if}Naslos v{__APP_VERSION__}
        </p>
      {/if}
    </div>
  </div>
</aside>
