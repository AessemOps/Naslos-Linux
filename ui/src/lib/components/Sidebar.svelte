<script lang="ts">
  import { page } from '$app/state';
  import { onMount } from 'svelte';
  import { mobileDrawer } from '$lib/actions/drawer';
  import { appUpdates, startAppUpdates } from '$lib/stores/appUpdates';
  import {
    closeMobileNav,
    mobileNavOpen,
    sidebarCollapsed,
    toggleMobileNav,
    toggleSidebar
  } from '$lib/stores/sidebar';
  import { refreshSystemUpdates, systemUpdates } from '$lib/stores/systemUpdates';
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
        { name: 'Replication', path: '/replication', icon: 'copy' },
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

  onMount(async () => {
    try {
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
  });

  onMount(() => {
    void refreshSystemUpdates();
  });

  // The update summary is shared with the Apps page (Installed tab badge) and
  // polls every 60s while the document is visible.
  onMount(() => {
    const stop = startAppUpdates();
    return stop;
  });

  // A resize past lg means the off-canvas drawer is no longer the active
  // surface; closing it keeps its dialog semantics and focus trap off desktop.
  onMount(() => {
    if (typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia('(min-width: 1024px)');
    const onChange = () => {
      if (mq.matches) closeMobileNav();
    };
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  });

  function onWindowKeydown(event: KeyboardEvent) {
    if (!$mobileNavOpen || event.key !== 'Escape') return;
    event.preventDefault();
    closeMobileNav();
  }

  // The session belongs to Authelia's forwardAuth portal, so signing out is a
  // POST to its logout endpoint (same origin, routed straight to the portal).
  // Only a confirmed response navigates away; a failed POST stays put and
  // offers the portal, so the UI never claims a session ended when it did not.
  let loggingOut = false;
  let logoutError = '';

  async function logout() {
    if (loggingOut) return;
    loggingOut = true;
    logoutError = '';
    try {
      const res = await fetch('/authelia/api/logout', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ targetURL: '/' })
      });
      if (!res.ok) {
        throw new Error(`Sign out failed (HTTP ${res.status}).`);
      }
      window.location.assign('/');
    } catch (e: any) {
      // Staying put is safer than redirecting into a still-authenticated
      // session. Offer the portal as the fallback sign-out path.
      logoutError = e?.message || 'Sign out failed; the session may still be active.';
      loggingOut = false;
    }
  }

  $: updateAvailable = $systemUpdates.available && $systemUpdates.steppingAllowed;
</script>

<svelte:window on:keydown={onWindowKeydown} />

<!-- Mobile top bar: the only shell chrome below lg. -->
<header
  class="sticky top-0 z-30 flex h-14 items-center gap-2 border-b border-naslos-border bg-naslos-surface/95 px-3 backdrop-blur lg:hidden"
>
  <button
    type="button"
    class="icon-btn text-gray-300 hover:bg-naslos-border hover:text-naslos-strong"
    aria-label="Open navigation"
    aria-expanded={$mobileNavOpen}
    aria-controls="sidebar-nav"
    on:click={toggleMobileNav}
  >
    <Icon name="menu" />
  </button>
  <a href="/" class="flex min-w-0 items-center gap-2" aria-label="Naslos home" on:click={closeMobileNav}>
    <img src="/logo.png" alt="Naslos logo" class="h-8 w-8 shrink-0" />
    <span class="truncate text-lg font-bold text-naslos-link">Naslos</span>
  </a>
</header>

{#if $mobileNavOpen}
  <div
    class="fixed inset-0 z-40 bg-black/60 lg:hidden"
    aria-hidden="true"
    on:click={closeMobileNav}
  ></div>
{/if}

<aside
  id="sidebar"
  class="fixed inset-y-0 left-0 z-40 flex w-64 max-w-[85vw] flex-col border-r border-naslos-border bg-naslos-surface transition-[transform,width] duration-200 lg:z-30 lg:max-w-none lg:visible lg:translate-x-0 {$mobileNavOpen
    ? 'visible translate-x-0'
    : 'invisible -translate-x-full'} {$sidebarCollapsed ? 'lg:w-16' : 'lg:w-64'}"
  role={$mobileNavOpen ? 'dialog' : undefined}
  aria-modal={$mobileNavOpen ? 'true' : undefined}
  aria-label={$mobileNavOpen ? 'Navigation' : undefined}
  use:mobileDrawer={{ open: $mobileNavOpen }}
>
  <div
    class="flex items-center gap-3 border-b border-naslos-border p-4 {$sidebarCollapsed
      ? 'lg:justify-center'
      : ''}"
  >
    <img src="/logo.png" alt="Naslos logo" class="h-10 w-10 shrink-0" />
    <div class="min-w-0 {$sidebarCollapsed ? 'lg:sr-only' : ''}">
      <span class="block truncate text-2xl font-bold leading-tight text-naslos-link">Naslos</span>
      <p class="mt-1 text-xs text-gray-500">Talos Linux NAS</p>
    </div>
    {#if !$sidebarCollapsed}
      <button
        type="button"
        class="icon-btn ml-auto hidden text-gray-400 hover:bg-naslos-border hover:text-naslos-strong lg:inline-flex"
        aria-label="Collapse the sidebar"
        title="Collapse the sidebar"
        aria-expanded={true}
        aria-controls="sidebar-nav"
        on:click={toggleSidebar}
      >
        <Icon name="chevrons-left" />
      </button>
    {/if}
    <button
      type="button"
      class="icon-btn ml-auto text-gray-400 hover:bg-naslos-border hover:text-naslos-strong lg:hidden"
      aria-label="Close navigation"
      on:click={closeMobileNav}
    >
      <Icon name="x" />
    </button>
  </div>

  {#if $sidebarCollapsed}
    <div class="hidden justify-center border-b border-naslos-border p-2 lg:flex">
      <button
        type="button"
        class="icon-btn text-gray-400 hover:bg-naslos-border hover:text-naslos-strong"
        aria-label="Expand the sidebar"
        title="Expand the sidebar"
        aria-expanded={!$sidebarCollapsed}
        aria-controls="sidebar-nav"
        on:click={toggleSidebar}
      >
        <Icon name="chevron-right" />
      </button>
    </div>
  {/if}

  <nav id="sidebar-nav" class="flex-1 space-y-4 overflow-y-auto p-2" aria-label="Main navigation">
    {#each navGroups as group}
      <div>
        <div
          class="px-3 pb-1 text-[11px] font-semibold uppercase tracking-wider text-gray-500 {$sidebarCollapsed
            ? 'lg:sr-only'
            : ''}"
        >
          {group.label}
        </div>
        <div class="space-y-0.5">
          {#each group.items as item}
            {@const active = activePath(page.url.pathname) === item.path}
            <a
              href={item.path}
              class="nav-link {$sidebarCollapsed ? 'lg:justify-center lg:px-2' : ''} {active
                ? 'nav-link-active'
                : 'text-gray-400 hover:bg-naslos-border/60 hover:text-naslos-strong'}"
              aria-current={active ? 'page' : undefined}
              title={$sidebarCollapsed ? item.name : undefined}
              on:click={closeMobileNav}
            >
              <Icon name={item.icon} />
              <span class="truncate {$sidebarCollapsed ? 'lg:sr-only' : ''}">{item.name}</span>
              {#if item.path === '/settings' && updateAvailable}
                <span
                  class="ml-auto h-2 w-2 shrink-0 rounded-full bg-naslos-accent {$sidebarCollapsed
                    ? 'lg:hidden'
                    : ''}"
                  role="status"
                  aria-label="A system update is available"
                  title="A system update is available"
                ></span>
              {:else if item.path === '/apps' && $appUpdates.count > 0}
                <span
                  class="ml-auto h-2 w-2 shrink-0 rounded-full bg-naslos-accent {$sidebarCollapsed
                    ? 'lg:hidden'
                    : ''}"
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
        class="nav-link text-gray-400 hover:bg-naslos-border/60 hover:text-naslos-strong {$sidebarCollapsed
          ? 'lg:justify-center lg:px-2'
          : ''}"
        title={$sidebarCollapsed ? 'Traefik' : undefined}
        on:click={closeMobileNav}
      >
        <Icon name="compass" />
        <span class="truncate {$sidebarCollapsed ? 'lg:sr-only' : ''}">Traefik</span>
        {#if !$sidebarCollapsed}
          <span class="ml-auto"><Icon name="external-link" size={14} /></span>
        {/if}
      </a>
    {/if}
  </nav>

  <div class="space-y-1 border-t border-naslos-border p-2">
    {#if logoutError}
      <p role="alert" class="break-words px-2 py-1 text-xs text-red-300">
        {logoutError}
        <a class="text-naslos-link underline" href="/authelia/" data-sveltekit-reload>Open the portal</a>
      </p>
    {/if}
    <button
      type="button"
      class="nav-link w-full text-gray-400 hover:bg-naslos-border/60 hover:text-naslos-strong disabled:cursor-not-allowed disabled:opacity-50 {$sidebarCollapsed
        ? 'lg:justify-center lg:px-2'
        : ''}"
      on:click={logout}
      disabled={loggingOut}
    >
      <Icon name="log-out" />
      {#if loggingOut}
        <span class="truncate {$sidebarCollapsed ? 'lg:sr-only' : ''}">Signing out…</span>
      {:else}
        <span class="truncate {$sidebarCollapsed ? 'lg:sr-only' : ''}">Sign out</span>
      {/if}
    </button>
    <div
      class="flex items-center gap-1 px-1 {$sidebarCollapsed
        ? 'lg:justify-center'
        : 'justify-between'}"
    >
      <ThemeToggle />
      <p class="truncate text-xs text-gray-500 {$sidebarCollapsed ? 'lg:sr-only' : ''}">
        {#if username}{username} · {/if}Naslos v{__APP_VERSION__}
      </p>
    </div>
  </div>
</aside>
