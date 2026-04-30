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
  <path d="M5 4h13a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5V4z"/>
  <path d="M5 4v18"/>
  <path d="M3 8h2"/>
  <path d="M3 12h2"/>
  <path d="M3 16h2"/>
  <circle cx="12" cy="11" r="2.2"/>
  <path d="M9 17c0-1.7 1.3-3 3-3s3 1.3 3 3"/>
</svg>
