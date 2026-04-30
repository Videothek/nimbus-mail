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
  <circle cx="12" cy="12" r="9"/>
  <path d="M8 14s1.5 2 4 2 4-2 4-2"/>
  <circle cx="9" cy="10" r="0.9" fill="currentColor" stroke="none"/>
  <circle cx="15" cy="10" r="0.9" fill="currentColor" stroke="none"/>
</svg>
