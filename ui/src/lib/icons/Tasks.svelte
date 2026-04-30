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
  <path d="M4 6l1.5 1.5L8 5"/>
  <path d="M4 12l1.5 1.5L8 11"/>
  <path d="M4 18l1.5 1.5L8 17"/>
  <path d="M11 6h9"/>
  <path d="M11 12h9"/>
  <path d="M11 18h6"/>
</svg>
