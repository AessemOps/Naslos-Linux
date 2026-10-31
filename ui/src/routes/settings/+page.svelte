<script lang="ts">
  const sections = [
    { href: '/users', title: 'Users', detail: 'Accounts, passwords and status', icon: '👤' },
    { href: '/groups', title: 'Groups', detail: 'Group membership', icon: '👥' },
    { href: '/shares', title: 'Shares', detail: 'SMB and NFS exports', icon: '📁' },
    { href: '/notifications', title: 'Notifications', detail: 'ntfy alerts and thresholds', icon: '🔔' },
    { href: '/dns', title: 'Dynamic DNS', detail: 'Providers and records', icon: '🌐' },
    { href: '/domains', title: 'Domains & TLS', detail: 'Base domains and certificates', icon: '🔐' },
    { href: '/pools', title: 'Pools', detail: 'ZFS pools and datasets', icon: '💾' },
    { href: '/disks', title: 'Disks', detail: 'Devices and pool creation', icon: '🩺' },
    { href: '/backups', title: 'Backups', detail: 'Buddy peers and schedules', icon: '☁️' },
    { href: '/apps', title: 'Apps', detail: 'Catalog, installs and exposure', icon: '🧩' },
    { href: '/datasets', title: 'Datasets', detail: 'Datasets apps may mount', icon: '🗂️' },
    { href: '/terminal', title: 'Terminal', detail: 'Pod logs and shells', icon: '⌨️' }
  ];
</script>

<div class="max-w-4xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Settings</h1>
  <p class="text-gray-400 mb-8">Every configuration area in one place.</p>

  <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
    {#each sections as section (section.href)}
      <a href={section.href} class="card hover:border-naslos-accent transition-colors">
        <div class="text-2xl mb-2" aria-hidden="true">{section.icon}</div>
        <h2 class="font-bold">{section.title}</h2>
        <p class="text-sm text-gray-400 mt-1">{section.detail}</p>
      </a>
    {/each}
  </div>
</div>
