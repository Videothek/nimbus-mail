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
  <path d="M10 6h11"/>
  <path d="M10 12h11"/>
  <path d="M10 18h11"/>
  <path d="M3 4.5L4.5 4v5"/>
  <path d="M3.5 9h2.5"/>
  <path d="M3 14.5a1.5 1.5 0 0 1 3 0c0 1.2-3 1.8-3 3.5h3"/>
</svg>
