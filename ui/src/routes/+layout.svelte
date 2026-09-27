<script lang="ts">
  import '../app.css';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import Toast from '$lib/components/Toast.svelte';
  import AppJobsDrawer from '$lib/components/AppJobsDrawer.svelte';
</script>

<div class="flex min-h-screen">
  <Sidebar />
  <main class="flex-1 ml-64 p-8">
    <slot />
  </main>
</div>

<AppJobsDrawer />
<Toast />
