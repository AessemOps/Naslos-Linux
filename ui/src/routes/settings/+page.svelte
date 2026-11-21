<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';
  import ThemeToggle from '$lib/components/ThemeToggle.svelte';
  import { theme } from '$lib/stores/theme';

  // A directory of every configuration area. The Appearance panel above the
  // grid is the only non-navigating entry: the theme toggle applies in place.
  const sections = [
    { href: '/system', title: 'System', detail: 'Versions and upgrades', icon: 'arrow-up' },
    { href: '/users', title: 'Users', detail: 'Accounts, passwords and status', icon: 'user' },
    { href: '/groups', title: 'Groups', detail: 'Group membership', icon: 'users' },
    { href: '/shares', title: 'Shares', detail: 'SMB and NFS exports', icon: 'folder' },
    { href: '/notifications', title: 'Notifications', detail: 'ntfy alerts and thresholds', icon: 'bell' },
    { href: '/settings/network', title: 'Network', detail: 'Links, addressing, MTU and DNS', icon: 'network' },
    { href: '/dns', title: 'Dynamic DNS', detail: 'Providers and records', icon: 'earth' },
    { href: '/domains', title: 'Domains & TLS', detail: 'Base domains and certificates', icon: 'lock' },
    { href: '/pools', title: 'Pools', detail: 'ZFS pools and datasets', icon: 'database' },
    { href: '/disks', title: 'Disks', detail: 'Devices and pool creation', icon: 'hard-drive' },
    { href: '/backups', title: 'Backups', detail: 'Buddy peers and schedules', icon: 'cloud' },
    { href: '/apps', title: 'Apps', detail: 'Catalog, installs and exposure', icon: 'puzzle' },
    { href: '/datasets', title: 'Datasets', detail: 'Datasets apps may mount', icon: 'folder-tree' },
    { href: '/settings/mcp', title: 'MCP', detail: 'AI assistant access and tokens', icon: 'bot' },
    { href: '/terminal', title: 'Terminal', detail: 'Pod logs and shells', icon: 'keyboard' }
  ];
</script>

<div class="max-w-4xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Settings</h1>
  <p class="text-gray-400 mb-8">Every configuration area in one place.</p>

  <div class="card mb-6 flex flex-wrap items-center justify-between gap-4">
    <div class="flex items-center gap-4">
      <div
        class="w-12 h-12 rounded-full bg-naslos-dark border border-naslos-border flex items-center justify-center text-gray-400 shrink-0"
        aria-hidden="true"
      >
        <Icon name={$theme === 'light' ? 'sun' : 'moon'} size={24} />
      </div>
      <div class="min-w-0">
        <h2 class="font-bold">Appearance</h2>
        <p class="text-sm text-gray-400 mt-0.5">
          Theme: {$theme === 'light' ? 'Light' : 'Dark'}. The choice is remembered on this
          browser; new visitors start from the system preference.
        </p>
      </div>
    </div>
    <ThemeToggle />
  </div>

  <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
    {#each sections as section (section.href)}
      <a href={section.href} class="card hover:border-naslos-accent transition-colors">
        <div class="text-gray-400 mb-2" aria-hidden="true">
          <Icon name={section.icon} size={24} />
        </div>
        <h2 class="font-bold">{section.title}</h2>
        <p class="text-sm text-gray-400 mt-1">{section.detail}</p>
      </a>
    {/each}
  </div>
</div>
