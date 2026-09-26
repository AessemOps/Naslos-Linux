<script lang="ts">
  import { page } from '$app/stores';
  import { onMount } from 'svelte';

  interface NavItem {
    name: string;
    path: string;
    icon: string;
  }

  const navItems: NavItem[] = [
    { name: 'Dashboard', path: '/', icon: '📊' },
    { name: 'Pools', path: '/pools', icon: '🗄️' },
    { name: 'Users', path: '/users', icon: '👤' },
    { name: 'Groups', path: '/groups', icon: '👥' },
    { name: 'Disks', path: '/disks', icon: '💾' },
    { name: 'Apps', path: '/apps', icon: '📦' },
    { name: 'Domains', path: '/domains', icon: '🌐' },
    { name: 'Dynamic DNS', path: '/dns', icon: '🌍' },
    { name: 'Shares', path: '/shares', icon: '🔗' },
    { name: 'Backups', path: '/backups', icon: '💾' },
    { name: 'Terminal', path: '/terminal', icon: '💻' },
    { name: 'Notifications', path: '/notifications', icon: '🔔' },
    { name: 'Settings', path: '/settings', icon: '⚙️' }
  ];

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

<aside class="fixed left-0 top-0 h-screen w-64 bg-naslos-surface border-r border-naslos-border flex flex-col">
  <div class="p-6 border-b border-naslos-border">
    <div class="flex items-center gap-3">
      <img src="/logo.png" alt="Naslos logo" class="w-10 h-10 shrink-0" />
      <div>
        <h1 class="text-2xl font-bold text-naslos-primary">Naslos</h1>
        <p class="text-xs text-gray-500 mt-1">Talos Linux NAS</p>
      </div>
    </div>
  </div>

  <nav class="flex-1 p-4 space-y-1">
    {#each navItems as item}
      <a
        href={item.path}
        class="flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors"
        class:bg-naslos-primary={$page.url.pathname === item.path}
        class:text-white={$page.url.pathname === item.path}
        class:text-gray-400={$page.url.pathname !== item.path}
        class:hover:bg-naslos-border={$page.url.pathname !== item.path}
      >
        <span class="text-lg">{item.icon}</span>
        <span class="font-medium">{item.name}</span>
      </a>
    {/each}

    {#if isAdmin}
      <a
        href="/traefik/dashboard/"
        target="_blank"
        rel="noreferrer"
        class="flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors text-gray-400 hover:bg-naslos-border"
      >
        <span class="text-lg">🧭</span>
        <span class="font-medium">Traefik</span>
        <span class="ml-auto text-xs text-gray-600">↗</span>
      </a>
    {/if}
  </nav>

  <div class="p-4 border-t border-naslos-border space-y-3">
    <button
      type="button"
      class="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors text-gray-400 hover:bg-naslos-border hover:text-white disabled:opacity-50"
      on:click={logout}
      disabled={loggingOut}
    >
      <span class="text-lg" aria-hidden="true">↩</span>
      <span class="font-medium">{loggingOut ? 'Signing out…' : 'Sign out'}</span>
    </button>
    <p class="text-xs text-gray-600 text-center">
      {#if username}{username} · {/if}Naslos v0.1.0
    </p>
  </div>
</aside>
