<script lang="ts">
  import { page } from '$app/stores';

  interface NavItem {
    name: string;
    path: string;
    icon: string;
  }

  const navItems: NavItem[] = [
    { name: 'Dashboard', path: '/', icon: '📊' },
    { name: 'Users', path: '/users', icon: '👤' },
    { name: 'Disks', path: '/disks', icon: '💾' },
    { name: 'Apps', path: '/apps', icon: '📦' },
    { name: 'Shares', path: '/shares', icon: '🔗' },
    { name: 'Terminal', path: '/terminal', icon: '💻' },
    { name: 'Notifications', path: '/notifications', icon: '🔔' },
    { name: 'Settings', path: '/settings', icon: '⚙️' }
  ];
</script>

<aside class="fixed left-0 top-0 h-screen w-64 bg-nasos-surface border-r border-nasos-border flex flex-col">
  <div class="p-6 border-b border-nasos-border">
    <h1 class="text-2xl font-bold text-nasos-primary">NasOS</h1>
    <p class="text-xs text-gray-500 mt-1">Talos Linux NAS</p>
  </div>

  <nav class="flex-1 p-4 space-y-1">
    {#each navItems as item}
      <a
        href={item.path}
        class="flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors"
        class:bg-nasos-primary={$page.url.pathname === item.path}
        class:text-white={$page.url.pathname === item.path}
        class:text-gray-400={$page.url.pathname !== item.path}
        class:hover:bg-nasos-border={$page.url.pathname !== item.path}
      >
        <span class="text-lg">{item.icon}</span>
        <span class="font-medium">{item.name}</span>
      </a>
    {/each}
  </nav>

  <div class="p-4 border-t border-nasos-border">
    <p class="text-xs text-gray-600 text-center">NasOS v0.1.0</p>
  </div>
</aside>
