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
  <rect x="11" y="4" width="10" height="16" rx="1"/>
  <path d="M11 10h10"/>
  <path d="M11 14h10"/>
  <path d="M16 10v10"/>
  <path d="M3 12h6"/>
  <path d="M6 9l-3 3 3 3"/>
</svg>
