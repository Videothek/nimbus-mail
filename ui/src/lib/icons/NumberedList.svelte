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
  <path d="M9 6h12"/>
  <path d="M9 12h12"/>
  <path d="M9 18h12"/>
  <path d="M3 5l1.5-1v4"/>
  <path d="M3 16h2.5a1 1 0 0 1 0 2H4l1.5-2a1 1 0 0 0-2.2-.5"/>
</svg>
