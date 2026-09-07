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
  <rect x="4" y="3.5" width="16" height="17" rx="2"/>
  <path d="M7.5 8.5h1.5M11.5 8.5h5"/>
  <path d="M7.5 12h1.5M11.5 12h5"/>
  <path d="M7.5 15.5h1.5M11.5 15.5h3"/>
</svg>
