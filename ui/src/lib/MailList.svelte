<script lang="ts">
  const emails = [
    {
      id: '1',
      from: 'Alice Johnson',
      subject: 'Project kickoff meeting',
      preview: 'Hi team, let\'s schedule the kickoff for next week...',
      date: '10:30 AM',
      unread: true,
      starred: false,
    },
    {
      id: '2',
      from: 'Bob Smith',
      subject: 'Re: Design review feedback',
      preview: 'Thanks for the feedback! I\'ve updated the mockups...',
      date: '9:15 AM',
      unread: true,
      starred: true,
    },
    {
      id: '3',
      from: 'Nextcloud',
      subject: 'New file shared with you',
      preview: 'Carol Davis shared "Q2 Report.pdf" with you...',
      date: 'Yesterday',
      unread: false,
      starred: false,
    },
    {
      id: '4',
      from: 'Dave Wilson',
      subject: 'Calendar invite: Sprint planning',
      preview: 'You\'ve been invited to Sprint Planning on Monday...',
      date: 'Yesterday',
      unread: false,
      starred: false,
    },
    {
      id: '5',
      from: 'Security Team',
      subject: 'Monthly security update',
      preview: 'Please review the latest security policies...',
      date: 'Apr 10',
      unread: false,
      starred: false,
    },
  ]

  let selectedId = $state('1')
</script>

<div class="w-80 shrink-0 border-r border-surface-200 dark:border-surface-700 flex flex-col">
  <!-- Search bar -->
  <div class="p-3 border-b border-surface-200 dark:border-surface-700">
    <input
      type="text"
      placeholder="Search mail..."
      class="input w-full px-3 py-2 text-sm rounded-md"
    />
  </div>

  <!-- Email list -->
  <div class="flex-1 overflow-y-auto">
    {#each emails as email}
      <button
        class="w-full text-left px-4 py-3 border-b border-surface-100 dark:border-surface-800 transition-colors
          {selectedId === email.id
            ? 'bg-primary-500/10'
            : 'hover:bg-surface-100 dark:hover:bg-surface-800'}"
        onclick={() => (selectedId = email.id)}
      >
        <div class="flex items-center justify-between mb-1">
          <span class="text-sm {email.unread ? 'font-semibold' : 'font-normal'}">
            {email.from}
          </span>
          <span class="text-xs text-surface-500">{email.date}</span>
        </div>
        <p class="text-sm {email.unread ? 'font-medium' : ''} truncate">{email.subject}</p>
        <p class="text-xs text-surface-500 truncate mt-0.5">{email.preview}</p>
      </button>
    {/each}
  </div>
</div>
