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
  <circle cx="12" cy="12" r="3.5"/>
  <path d="M12 3v2"/>
  <path d="M12 19v2"/>
  <path d="M3 12h2"/>
  <path d="M19 12h2"/>
  <path d="M5.6 5.6l1.4 1.4"/>
  <path d="M17 17l1.4 1.4"/>
  <path d="M5.6 18.4l1.4-1.4"/>
  <path d="M17 7l1.4-1.4"/>
</svg>
