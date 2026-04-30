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
  <circle cx="4.5" cy="6" r="1.4" fill="currentColor" stroke="none"/>
  <circle cx="4.5" cy="12" r="1.4" fill="currentColor" stroke="none"/>
  <circle cx="4.5" cy="18" r="1.4" fill="currentColor" stroke="none"/>
</svg>
