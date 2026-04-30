<script lang="ts">
  export let size: number | string = 20;
  let className = '';
  export { className as class };
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.6"
  stroke-linecap="round"
  stroke-linejoin="round"
  class={className}
  aria-hidden="true"
>
  <rect x="3" y="4" width="10" height="16" rx="1"/>
  <path d="M3 10h10"/>
  <path d="M3 14h10"/>
  <path d="M8 4v16"/>
  <path d="M15 12h6"/>
  <path d="M18 9l3 3-3 3"/>
</svg>
