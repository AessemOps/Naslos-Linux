<script lang="ts">
  import { onMount } from 'svelte';

  interface App {
    name: string;
    namespace: string;
    chart: string;
    version: string;
    status: string;
    updatedAt: string;
    description: string;
  }

  let apps: App[] = [];
  let loading = true;

  async function loadApps() {
    loading = true;
    try {
      const res = await fetch('/api/apps');
      apps = await res.json();
    } catch (e) {
      console.error('Failed to load apps:', e);
    } finally {
      loading = false;
    }
  }

  async function uninstallApp(name: string) {
    if (!confirm(`Are you sure you want to uninstall ${name}? This will remove all app data.`)) {
      return;
    }
    try {
      await fetch(`/api/apps/${name}`, { method: 'DELETE' });
      loadApps();
    } catch (e) {
      console.error('Failed to uninstall:', e);
    }
  }

  function statusColor(status: string): string {
    switch (status) {
      case 'running': return 'text-green-400 bg-green-900/50';
      case 'failed': return 'text-red-400 bg-red-900/50';
      case 'pending': return 'text-yellow-400 bg-yellow-900/50';
      case 'stopped': return 'text-gray-400 bg-gray-900/50';
      default: return 'text-gray-400 bg-gray-900/50';
    }
  }

  onMount(loadApps);
</script>

<div>
  {#if loading}
    <p class="text-gray-400">Loading installed apps...</p>
  {:else if apps.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">📦</div>
      <h2 class="text-xl font-bold mb-2">No Apps Installed</h2>
      <p class="text-gray-400 mb-4">Browse the catalog to install your first app.</p>
      <a href="/apps" class="btn btn-primary">Browse Catalog</a>
    </div>
  {:else}
    <div class="space-y-3">
      {#each apps as app}
        <div class="card flex items-center gap-4">
          <div class="flex-1">
            <div class="flex items-center gap-3 mb-1">
              <h3 class="font-bold">{app.name}</h3>
              <span class={`text-xs px-2 py-0.5 rounded ${app.status === 'running' ? 'bg-green-900/50 text-green-400' : app.status === 'failed' ? 'bg-red-900/50 text-red-400' : app.status === 'pending' ? 'bg-yellow-900/50 text-yellow-400' : 'bg-gray-900/50 text-gray-400'}`}>
                {app.status}
              </span>
            </div>
            <p class="text-sm text-gray-400">{app.description || app.chart}</p>
            <p class="text-xs text-gray-500 mt-1">v{app.version} • {app.chart}</p>
          </div>
          <div class="flex gap-2">
            <button class="btn btn-secondary" disabled>Configure</button>
            <button class="btn btn-danger" on:click={() => uninstallApp(app.name)}>Uninstall</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
