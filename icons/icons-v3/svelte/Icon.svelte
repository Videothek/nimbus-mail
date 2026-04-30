<script lang="ts">
  /**
   * Generic icon component for the mail client.
   *
   * Usage:
   *   <Icon name="compose" />
   *   <Icon name="success" size={20} />
   *   <Icon name="loading" class="text-blue-500" />
   *
   * All icons inherit `currentColor` for stroke, so set color via CSS.
   */

  // Set v1 (18)
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

  // Set v2 (31)
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

  // Set v3 (21)
  import Online from './icons/Online.svelte';
  import Offline from './icons/Offline.svelte';
  import Typing from './icons/Typing.svelte';
  import Away from './icons/Away.svelte';
  import Help from './icons/Help.svelte';
  import Info from './icons/Info.svelte';
  import Warning from './icons/Warning.svelte';
  import Error from './icons/Error.svelte';
  import Success from './icons/Success.svelte';
  import Translate from './icons/Translate.svelte';
  import Spellcheck from './icons/Spellcheck.svelte';
  import Dictate from './icons/Dictate.svelte';
  import SaveDraft from './icons/SaveDraft.svelte';
  import OpenLink from './icons/OpenLink.svelte';
  import OpenOnDesktop from './icons/OpenOnDesktop.svelte';
  import Cloud from './icons/Cloud.svelte';
  import Today from './icons/Today.svelte';
  import NavBackward from './icons/NavBackward.svelte';
  import NavForward from './icons/NavForward.svelte';
  import ArrowLeft from './icons/ArrowLeft.svelte';
  import ArrowRight from './icons/ArrowRight.svelte';

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
    | 'encrypted' | 'signed' | 'verified'
    // v3
    | 'online' | 'offline' | 'typing' | 'away'
    | 'help' | 'info' | 'warning' | 'error' | 'success'
    | 'translate' | 'spellcheck' | 'dictate'
    | 'save-draft' | 'open-link' | 'open-on-desktop' | 'cloud' | 'today'
    | 'nav-backward' | 'nav-forward' | 'arrow-left' | 'arrow-right';

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
    // v3
    'online': Online,
    'offline': Offline,
    'typing': Typing,
    'away': Away,
    'help': Help,
    'info': Info,
    'warning': Warning,
    'error': Error,
    'success': Success,
    'translate': Translate,
    'spellcheck': Spellcheck,
    'dictate': Dictate,
    'save-draft': SaveDraft,
    'open-link': OpenLink,
    'open-on-desktop': OpenOnDesktop,
    'cloud': Cloud,
    'today': Today,
    'nav-backward': NavBackward,
    'nav-forward': NavForward,
    'arrow-left': ArrowLeft,
    'arrow-right': ArrowRight,
  };

  $: Component = map[name];
</script>

{#if Component}
  <svelte:component this={Component} {size} class={className} />
{/if}
