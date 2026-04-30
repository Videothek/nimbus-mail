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
  <path d="M12 3l2.6 5.6 6 .7-4.4 4.2 1.2 6.1L12 16.8l-5.4 2.8 1.2-6.1L3.4 9.3l6-.7L12 3z"/>
</svg>
