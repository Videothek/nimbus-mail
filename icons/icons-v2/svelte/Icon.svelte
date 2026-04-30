<script lang="ts">
  /**
   * Generic icon component for the mail client.
   *
   * Usage:
   *   <Icon name="compose" />
   *   <Icon name="rsvp-accept" size={20} />
   *   <Icon name="loading" class="text-blue-500" />
   *
   * All icons inherit `currentColor` for stroke, so set color via CSS:
   *   .toolbar-btn { color: var(--text-secondary); }
   *   .toolbar-btn:hover { color: var(--text-primary); }
   */

  // Set v1
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

  // Set v2
  import Reply from './icons/Reply.svelte';
  import ReplyAll from './icons/ReplyAll.svelte';
  import Forward from './icons/Forward.svelte';
  import Archive from './icons/Archive.svelte';
  import Snooze from './icons/Snooze.svelte';
  import Flag from './icons/Flag.svelte';
  import Star from './icons/Star.svelte';
  import Search from './icons/Search.svelte';
  import Filter from './icons/Filter.svelte';
  import Sort from './icons/Sort.svelte';
  import Attachment from './icons/Attachment.svelte';
  import Download from './icons/Download.svelte';
  import Print from './icons/Print.svelte';
  import Spam from './icons/Spam.svelte';
  import Block from './icons/Block.svelte';
  import Important from './icons/Important.svelte';
  import Drafts from './icons/Drafts.svelte';
  import Sent from './icons/Sent.svelte';
  import Trash from './icons/Trash.svelte';
  import Refresh from './icons/Refresh.svelte';
  import Sync from './icons/Sync.svelte';
  import Loading from './icons/Loading.svelte';
  import AddAccount from './icons/AddAccount.svelte';
  import SignOut from './icons/SignOut.svelte';
  import Lock from './icons/Lock.svelte';
  import Notification from './icons/Notification.svelte';
  import Mute from './icons/Mute.svelte';
  import DoNotDisturb from './icons/DoNotDisturb.svelte';
  import Encrypted from './icons/Encrypted.svelte';
  import Signed from './icons/Signed.svelte';
  import Verified from './icons/Verified.svelte';

  export type IconName =
    // v1
    | 'compose' | 'contacts' | 'calendar' | 'meetings'
    | 'global-inbox' | 'files' | 'notes' | 'tasks'
    | 'share-links' | 'settings' | 'read' | 'unread'
    | 'move-to-folder' | 'delete'
    | 'rsvp-accept' | 'rsvp-tentative' | 'rsvp-decline' | 'rsvp-canceled'
    // v2
    | 'reply' | 'reply-all' | 'forward'
    | 'archive' | 'snooze' | 'flag' | 'star'
    | 'search' | 'filter' | 'sort'
    | 'attachment' | 'download' | 'print'
    | 'spam' | 'block' | 'important'
    | 'drafts' | 'sent' | 'trash'
    | 'refresh' | 'sync' | 'loading'
    | 'add-account' | 'sign-out' | 'lock'
    | 'notification' | 'mute' | 'do-not-disturb'
    | 'encrypted' | 'signed' | 'verified';

  export let name: IconName;
  export let size: number | string = 20;
  let className = '';
  export { className as class };

  const map = {
    // v1
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
    // v2
    'reply': Reply,
    'reply-all': ReplyAll,
    'forward': Forward,
    'archive': Archive,
    'snooze': Snooze,
    'flag': Flag,
    'star': Star,
    'search': Search,
    'filter': Filter,
    'sort': Sort,
    'attachment': Attachment,
    'download': Download,
    'print': Print,
    'spam': Spam,
    'block': Block,
    'important': Important,
    'drafts': Drafts,
    'sent': Sent,
    'trash': Trash,
    'refresh': Refresh,
    'sync': Sync,
    'loading': Loading,
    'add-account': AddAccount,
    'sign-out': SignOut,
    'lock': Lock,
    'notification': Notification,
    'mute': Mute,
    'do-not-disturb': DoNotDisturb,
    'encrypted': Encrypted,
    'signed': Signed,
    'verified': Verified,
  };

  $: Component = map[name];
</script>

{#if Component}
  <svelte:component this={Component} {size} class={className} />
{/if}
