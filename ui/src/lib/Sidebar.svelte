<script lang="ts">
  // Props passed from App.svelte
  interface Props {
    onsettings: () => void
  }
  let { onsettings }: Props = $props()

  const folders = [
    { name: 'Inbox', icon: '📥', count: 12 },
    { name: 'Sent', icon: '📤', count: 0 },
    { name: 'Drafts', icon: '📝', count: 3 },
    { name: 'Starred', icon: '⭐', count: 5 },
    { name: 'Trash', icon: '🗑️', count: 0 },
  ]

  const integrations = [
    { name: 'Calendar', icon: '📅' },
    { name: 'Contacts', icon: '👤' },
    { name: 'Nextcloud Talk', icon: '💬' },
    { name: 'Nextcloud Files', icon: '📁' },
  ]

  let activeFolder = $state('Inbox')
</script>

<aside class="w-56 shrink-0 border-r border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800 flex flex-col">
  <!-- App title -->
  <div class="p-4 border-b border-surface-200 dark:border-surface-700">
    <h1 class="text-lg font-bold text-primary-500">Nimbus Mail</h1>
  </div>

  <!-- Compose button -->
  <div class="p-3">
    <button class="btn preset-filled-primary-500 w-full">
      Compose
    </button>
  </div>

  <!-- Mail folders -->
  <nav class="flex-1 overflow-y-auto px-2">
    <p class="px-2 py-1 text-xs font-semibold text-surface-500 uppercase tracking-wider">Folders</p>
    {#each folders as folder}
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors
          {activeFolder === folder.name
            ? 'bg-primary-500/10 text-primary-500 font-medium'
            : 'hover:bg-surface-200 dark:hover:bg-surface-700'}"
        onclick={() => (activeFolder = folder.name)}
      >
        <span>{folder.icon}</span>
        <span class="flex-1 text-left">{folder.name}</span>
        {#if folder.count > 0}
          <span class="badge preset-filled-primary-500 text-xs">{folder.count}</span>
        {/if}
      </button>
    {/each}

    <hr class="my-3 border-surface-200 dark:border-surface-700" />

    <p class="px-2 py-1 text-xs font-semibold text-surface-500 uppercase tracking-wider">Integrations</p>
    {#each integrations as item}
      <button
        class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm hover:bg-surface-200 dark:hover:bg-surface-700 transition-colors"
      >
        <span>{item.icon}</span>
        <span class="flex-1 text-left">{item.name}</span>
      </button>
    {/each}
  </nav>

  <!-- Account / Settings -->
  <div class="p-3 border-t border-surface-200 dark:border-surface-700">
    <button
      class="w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm hover:bg-surface-200 dark:hover:bg-surface-700 transition-colors text-surface-500"
      onclick={onsettings}
    >
      <span>&#9881;</span>
      <span>Account Settings</span>
    </button>
  </div>
</aside>
