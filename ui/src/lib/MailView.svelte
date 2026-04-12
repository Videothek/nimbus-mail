<script lang="ts">
  // Placeholder content for the reading pane
</script>

<main class="flex-1 flex flex-col overflow-hidden">
  <!-- Email header -->
  <div class="p-6 border-b border-surface-200 dark:border-surface-700">
    <div class="flex items-start justify-between mb-2">
      <h2 class="text-xl font-semibold">Project kickoff meeting</h2>
      <span class="text-sm text-surface-500">10:30 AM</span>
    </div>
    <div class="flex items-center gap-2 text-sm text-surface-600 dark:text-surface-400">
      <span class="font-medium">Alice Johnson</span>
      <span>&lt;alice@example.com&gt;</span>
    </div>
    <div class="text-xs text-surface-500 mt-1">
      To: me, team@example.com
    </div>
  </div>

  <!-- Action bar -->
  <div class="flex items-center gap-2 px-6 py-2 border-b border-surface-200 dark:border-surface-700 text-sm">
    <button class="btn btn-sm preset-outlined-surface-500">Reply</button>
    <button class="btn btn-sm preset-outlined-surface-500">Reply All</button>
    <button class="btn btn-sm preset-outlined-surface-500">Forward</button>
    <div class="flex-1"></div>
    <button class="btn btn-sm preset-outlined-surface-500">Archive</button>
    <button class="btn btn-sm preset-outlined-surface-500">Delete</button>
  </div>

  <!-- Email body -->
  <div class="flex-1 overflow-y-auto p-6">
    <div class="prose dark:prose-invert max-w-none">
      <p>Hi team,</p>
      <p>
        I'd like to schedule our project kickoff meeting for next week.
        Please let me know your availability for Monday or Tuesday afternoon.
      </p>
      <p>
        We'll cover the project scope, timeline, and assign initial tasks.
        I've shared the project brief on Nextcloud — you can find it in the
        shared "Nimbus" folder.
      </p>
      <p>Looking forward to getting started!</p>
      <p>Best,<br />Alice</p>
    </div>
  </div>
</main>
