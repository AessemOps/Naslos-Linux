<script lang="ts">
  export let values: number[] = [];
  export let height = 28;

  $: points = toPoints(values, height);

  function toPoints(list: number[], h: number): string {
    if (list.length < 2) return '';
    const min = Math.min(...list);
    const max = Math.max(...list);
    const span = max - min || 1;
    const step = 100 / (list.length - 1);
    return list
      .map((value, index) => {
        const x = (index * step).toFixed(2);
        const y = (h - ((value - min) / span) * (h - 2) - 1).toFixed(2);
        return `${x},${y}`;
      })
      .join(' ');
  }
</script>

<svg
  viewBox={`0 0 100 ${height}`}
  preserveAspectRatio="none"
  class="w-full text-naslos-accent"
  style={`height:${height}px`}
  aria-hidden="true"
>
  {#if points}
    <polyline
      {points}
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linejoin="round"
      stroke-linecap="round"
      vector-effect="non-scaling-stroke"
    />
  {/if}
</svg>
