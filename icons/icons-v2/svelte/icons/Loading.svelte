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
  class="loading {className}"
  aria-hidden="true"
>
  <circle cx="12" cy="12" r="9" opacity="0.25"/>
  <path d="M12 3a9 9 0 0 1 9 9" stroke-width="2.4"/>
</svg>

<style>
  .loading {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
