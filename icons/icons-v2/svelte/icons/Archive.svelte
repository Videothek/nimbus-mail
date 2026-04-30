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
  <path d="M3 6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v3H3V6z"/>
  <path d="M3 9v9a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V9"/>
  <path d="M10 13h4"/>
</svg>
