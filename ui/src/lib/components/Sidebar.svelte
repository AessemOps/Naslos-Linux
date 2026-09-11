<script lang="ts">
  import { page } from '$app/stores';

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
    { name: 'Shares', path: '/shares', icon: '🔗' },
    { name: 'Terminal', path: '/terminal', icon: '💻' },
    { name: 'Notifications', path: '/notifications', icon: '🔔' },
    { name: 'Settings', path: '/settings', icon: '⚙️' }
  ];
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
  </nav>

  <div class="p-4 border-t border-naslos-border">
    <p class="text-xs text-gray-600 text-center">Naslos v0.1.0</p>
  </div>
</aside>
