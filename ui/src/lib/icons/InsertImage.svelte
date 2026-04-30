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
  <rect x="3" y="4" width="18" height="16" rx="2"/>
  <circle cx="9" cy="10" r="2"/>
  <path d="M3 17l5-5 4 4 3-3 6 6"/>
</svg>
