<script lang="ts">
  import '../app.css';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import Toast from '$lib/components/Toast.svelte';
  import AppJobsDrawer from '$lib/components/AppJobsDrawer.svelte';
  import { sidebarCollapsed } from '$lib/stores/sidebar';
</script>

<div class="flex min-h-screen">
  <Sidebar />
  <main
    class="flex-1 min-w-0 p-8 transition-[margin-left] duration-200"
    style="margin-left: {$sidebarCollapsed ? '4rem' : '16rem'}"
  >
    <slot />
  </main>
</div>

<AppJobsDrawer />
<Toast />
