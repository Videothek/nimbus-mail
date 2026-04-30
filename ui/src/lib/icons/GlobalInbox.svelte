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
  <path d="M3 13l3-8h12l3 8"/>
  <path d="M3 13v5a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-5"/>
  <path d="M3 13h5l1 2h6l1-2h5"/>
  <circle cx="12" cy="9" r="0.8" fill="currentColor"/>
  <circle cx="9" cy="9" r="0.8" fill="currentColor"/>
  <circle cx="15" cy="9" r="0.8" fill="currentColor"/>
</svg>
