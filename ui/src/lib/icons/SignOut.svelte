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
  <path d="M9 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h4"/>
  <path d="M16 8l4 4-4 4"/>
  <path d="M20 12H9"/>
</svg>
