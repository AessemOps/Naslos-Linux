<script lang="ts">
  import '../app.css';
  // Self-hosted fonts: no CDN and no network access at runtime, so the
  // appliance still renders correctly offline (CSP font-src 'self').
  import '@fontsource/fira-sans/400.css';
  import '@fontsource/fira-sans/500.css';
  import '@fontsource/fira-sans/600.css';
  import '@fontsource/fira-sans/700.css';
  import '@fontsource/fira-code/400.css';
  import '@fontsource/fira-code/500.css';
  import { afterNavigate } from '$app/navigation';
  import AppJobsDrawer from '$lib/components/AppJobsDrawer.svelte';
  import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';
  import Sidebar from '$lib/components/Sidebar.svelte';
  import { resolveConfirm } from '$lib/stores/confirm';
  import Toast from '$lib/components/Toast.svelte';
  import { closeMobileNav, sidebarCollapsed } from '$lib/stores/sidebar';

  // Move focus to the content on every client-side navigation, but not on the
  // initial load: stealing focus there would skip the page the browser is
  // still painting. The drawer always closes on navigation.
  afterNavigate((navigation) => {
    closeMobileNav();
    resolveConfirm(false);
    if (navigation.type === 'enter') return;
    document.getElementById('main-content')?.focus();
  });
</script>

<a href="#main-content" class="skip-link">Skip to main content</a>

<Sidebar />

<div
  class="min-h-screen transition-[padding] duration-200 {$sidebarCollapsed
    ? 'lg:pl-16'
    : 'lg:pl-64'}"
>
  <main id="main-content" tabindex="-1" class="p-4 focus:outline-none sm:p-6 lg:p-8">
    <slot />
  </main>
</div>

<AppJobsDrawer />
<Toast />
<ConfirmDialog />
