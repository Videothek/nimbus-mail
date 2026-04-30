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
  <path d="M6 16V11a6 6 0 0 1 12 0v5l2 2H4l2-2z"/>
  <path d="M10 20a2 2 0 0 0 4 0"/>
  <path d="M3 3l18 18"/>
</svg>
