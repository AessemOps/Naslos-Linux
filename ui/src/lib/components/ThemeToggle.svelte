<script lang="ts">
  import Icon from './Icon.svelte';
  import { theme, toggleTheme } from '$lib/stores/theme';

  $: light = $theme === 'light';
</script>

<button
  type="button"
  class="icon-btn text-gray-400 hover:text-naslos-strong hover:bg-naslos-border"
  aria-pressed={light}
  aria-label={light ? 'Switch to the dark theme' : 'Switch to the light theme'}
  title={light ? 'Switch to the dark theme' : 'Switch to the light theme'}
  on:click={toggleTheme}
>
  <Icon name={light ? 'sun' : 'moon'} />
</button>
