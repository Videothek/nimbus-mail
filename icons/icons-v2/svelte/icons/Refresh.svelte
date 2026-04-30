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
  <path d="M4 11a8 8 0 0 1 14-5l2 2"/>
  <path d="M20 13a8 8 0 0 1-14 5l-2-2"/>
  <path d="M20 4v4h-4"/>
  <path d="M4 20v-4h4"/>
</svg>
