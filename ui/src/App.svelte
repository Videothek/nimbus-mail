<script lang="ts">
  import Sidebar from './lib/Sidebar.svelte'
  import MailList from './lib/MailList.svelte'
  import MailView from './lib/MailView.svelte'
</script>

<div class="h-full flex">
  <!-- Sidebar: folders & navigation -->
  <Sidebar />

  <!-- Mail list -->
  <MailList />

  <!-- Mail content / reading pane -->
  <MailView />
</div>
