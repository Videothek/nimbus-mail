<script lang="ts">
  /**
   * Generic icon component for the mail client.
   *
   * Usage:
   *   <Icon name="compose" />
   *   <Icon name="rsvp-accept" size={20} />
   *   <Icon name="delete" class="text-red-500" />
   *
   * All icons inherit `currentColor` for stroke, so set color via CSS:
   *   .toolbar-btn { color: var(--text-secondary); }
   *   .toolbar-btn:hover { color: var(--text-primary); }
   */

  import Compose from './icons/Compose.svelte';
  import Contacts from './icons/Contacts.svelte';
  import Calendar from './icons/Calendar.svelte';
  import Meetings from './icons/Meetings.svelte';
  import GlobalInbox from './icons/GlobalInbox.svelte';
  import Files from './icons/Files.svelte';
  import Notes from './icons/Notes.svelte';
  import Tasks from './icons/Tasks.svelte';
  import ShareLinks from './icons/ShareLinks.svelte';
  import Settings from './icons/Settings.svelte';
  import Read from './icons/Read.svelte';
  import Unread from './icons/Unread.svelte';
  import MoveToFolder from './icons/MoveToFolder.svelte';
  import Delete from './icons/Delete.svelte';
  import RsvpAccept from './icons/RsvpAccept.svelte';
  import RsvpTentative from './icons/RsvpTentative.svelte';
  import RsvpDecline from './icons/RsvpDecline.svelte';
  import RsvpCanceled from './icons/RsvpCanceled.svelte';

  export type IconName =
    | 'compose'
    | 'contacts'
    | 'calendar'
    | 'meetings'
    | 'global-inbox'
    | 'files'
    | 'notes'
    | 'tasks'
    | 'share-links'
    | 'settings'
    | 'read'
    | 'unread'
    | 'move-to-folder'
    | 'delete'
    | 'rsvp-accept'
    | 'rsvp-tentative'
    | 'rsvp-decline'
    | 'rsvp-canceled';

  export let name: IconName;
  export let size: number | string = 20;
  let className = '';
  export { className as class };

  const map = {
    'compose': Compose,
    'contacts': Contacts,
    'calendar': Calendar,
    'meetings': Meetings,
    'global-inbox': GlobalInbox,
    'files': Files,
    'notes': Notes,
    'tasks': Tasks,
    'share-links': ShareLinks,
    'settings': Settings,
    'read': Read,
    'unread': Unread,
    'move-to-folder': MoveToFolder,
    'delete': Delete,
    'rsvp-accept': RsvpAccept,
    'rsvp-tentative': RsvpTentative,
    'rsvp-decline': RsvpDecline,
    'rsvp-canceled': RsvpCanceled,
  };

  $: Component = map[name];
</script>

{#if Component}
  <svelte:component this={Component} {size} class={className} />
{/if}
