<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
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
    { href: '/disks/health', title: 'Disk Health', detail: 'SMART and vdev health', icon: 'heart-pulse' },
    { href: '/replication', title: 'Replication', detail: 'Local dataset copies', icon: 'copy' },
    { href: '/backups', title: 'Backups', detail: 'Buddy peers and schedules', icon: 'cloud' },
    { href: '/apps', title: 'Apps', detail: 'Catalog, installs and exposure', icon: 'puzzle' },
    { href: '/vms', title: 'Virtual Machines', detail: 'QEMU guests and consoles', icon: 'monitor' },
    { href: '/datasets', title: 'Datasets', detail: 'Datasets apps may mount', icon: 'folder-tree' },
    { href: '/settings/mcp', title: 'MCP', detail: 'AI assistant access and tokens', icon: 'bot' },
    { href: '/terminal', title: 'Terminal', detail: 'Pod logs and shells', icon: 'keyboard' }
  ];
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader title="Settings" subtitle="Every configuration area in one place." />

  <section class="card mb-6 flex flex-wrap items-center justify-between gap-4" aria-labelledby="settings-appearance">
    <div class="flex min-w-0 flex-1 items-center gap-4">
      <div
        class="flex h-12 w-12 shrink-0 items-center justify-center rounded-full border border-naslos-border bg-naslos-dark text-gray-400"
        aria-hidden="true"
      >
        <Icon name={$theme === 'light' ? 'sun' : 'moon'} size={24} />
      </div>
      <div class="min-w-0">
        <h2 id="settings-appearance" class="font-bold">Appearance</h2>
        <p class="mt-0.5 text-sm text-gray-400">
          Theme: {$theme === 'light' ? 'Light' : 'Dark'}. The choice is remembered on this
          browser; new visitors start from the system preference.
        </p>
      </div>
    </div>
    <ThemeToggle />
  </section>

  <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
    {#each sections as section (section.href)}
      <a
        href={section.href}
        class="card flex items-start gap-4 transition-colors hover:border-naslos-accent"
      >
        <div
          class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg border border-naslos-border bg-naslos-dark text-gray-400"
          aria-hidden="true"
        >
          <Icon name={section.icon} size={20} />
        </div>
        <div class="min-w-0">
          <h2 class="font-bold">{section.title}</h2>
          <p class="mt-1 text-sm text-gray-400">{section.detail}</p>
        </div>
      </a>
    {/each}
  </div>
</div>
