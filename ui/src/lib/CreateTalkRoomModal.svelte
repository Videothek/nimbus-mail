<script lang="ts">
  /**
   * CreateTalkRoomModal — modal for creating a new Nextcloud Talk room.
   *
   * Two callers today:
   *   - **TalkView's "+ New room"** — opens with empty fields.
   *   - **MailView's "Talk" action** — opens with the email's subject as
   *     the room name and the thread's participants (From + To + Cc)
   *     pre-filled, satisfying issue #13's "create a Talk room from an
   *     email thread" task.
   *
   * Participants are entered via the same `AddressAutocomplete` the
   * Compose form uses, so picking from contacts works the same way.
   * Each comma-separated address is sent to the backend as a Talk
   * `email`-source participant — Talk emails them an invite link if
   * the address doesn't match a Nextcloud account on the server, and
   * promotes them to a full participant if it does. We don't need to
   * pre-resolve which is which on the client.
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'
  import AddressAutocomplete from './AddressAutocomplete.svelte'

  /** Mirrors the Rust `TalkRoom` Tauri-command return type. */
  export interface TalkRoom {
    token: string
    display_name: string
    room_type: 'one_to_one' | 'group' | 'public' | 'changelog' | 'other'
    unread_messages: number
    unread_mention: boolean
    last_activity: number
    web_url: string
  }

  interface Props {
    /** Nextcloud account id to create the room under. */
    ncId: string
    /** Pre-fill the room name (e.g. an email subject). */
    initialName?: string
    /**
     * Pre-fill participants. Each entry is a bare email address or an
     * RFC `"Name" <addr>` string — both shapes are accepted by the
     * same parser the EventEditor / Compose use. The backend only
     * sees the bare address.
     */
    initialParticipants?: string[]
    onclose: () => void
    /**
     * Fires once the room is created. Carries the freshly minted room
     * so the caller can refresh its list and/or open Compose with
     * the room link pre-filled.
     */
    oncreated: (room: TalkRoom) => void
  }
  const { ncId, initialName = '', initialParticipants = [], onclose, oncreated }: Props = $props()

  // svelte-ignore state_referenced_locally
  let roomName = $state(initialName)
  // svelte-ignore state_referenced_locally
  let participantsText = $state(initialParticipants.join(', '))
  let creating = $state(false)
  let error = $state('')

  /**
   * Strip an `"Name" <addr>` wrapper down to the bare address. Same
   * shape parser as `EventEditor.parseAddress`, kept inline so this
   * modal doesn't pull in the editor's helpers.
   */
  function bareEmail(piece: string): string | null {
    const trimmed = piece.trim()
    if (!trimmed) return null
    const m = trimmed.match(/^\s*(?:"[^"]*"|[^<]*?)\s*<([^>]+)>\s*$/)
    if (m) return m[1].trim()
    return trimmed
  }

  function buildParticipants() {
    const out: { kind: 'email'; value: string }[] = []
    const seen = new Set<string>()
    for (const piece of participantsText.split(/[,;]/)) {
      const addr = bareEmail(piece)
      if (!addr) continue
      const key = addr.toLowerCase()
      if (seen.has(key)) continue
      seen.add(key)
      // Talk's `emails` source covers both NC users and external
      // invitees: the server matches by email and promotes to a full
      // participant if a Nextcloud user owns that address.
      out.push({ kind: 'email', value: addr })
    }
    return out
  }

  async function create() {
    error = ''
    const name = roomName.trim()
    if (!name) {
      error = 'Room name is required.'
      return
    }
    creating = true
    try {
      const room = await invoke<TalkRoom>('create_talk_room', {
        ncId,
        roomName: name,
        participants: buildParticipants(),
      })
      oncreated(room)
      onclose()
    } catch (e) {
      error = formatError(e) || 'Failed to create Talk room'
    } finally {
      creating = false
    }
  }
</script>

<div
  class="fixed inset-0 z-60 flex items-center justify-center bg-black/50"
  role="dialog"
  aria-modal="true"
>
  <div class="w-[520px] max-h-[90vh] bg-surface-50 dark:bg-surface-900 rounded-lg shadow-xl flex flex-col">
    <header class="px-5 py-3 border-b border-surface-200 dark:border-surface-700 flex items-center justify-between">
      <h2 class="text-base font-semibold">New Talk room</h2>
      <button
        class="text-surface-500 hover:text-surface-900 dark:hover:text-surface-100"
        onclick={onclose}
        aria-label="Close"
      >✕</button>
    </header>

    <div class="flex-1 overflow-y-auto p-5 space-y-3">
      <div class="flex items-center gap-2">
        <label class="text-xs w-24 text-surface-500" for="talk-room-name">Room name</label>
        <input
          id="talk-room-name"
          class="input flex-1 px-3 py-2 text-sm rounded-md"
          bind:value={roomName}
          placeholder="Project sync"
          autocomplete="off"
        />
      </div>

      <div class="flex items-start gap-2">
        <label class="text-xs w-24 text-surface-500 pt-2" for="talk-room-participants">Invite</label>
        <AddressAutocomplete
          id="talk-room-participants"
          bind:value={participantsText}
          placeholder="alice@example.com, bob@example.com"
        />
      </div>

      <p class="text-xs text-surface-500 pl-26">
        Each address gets a Talk invite. Recipients with a Nextcloud account on this server join directly; others get an invite link by email.
      </p>

      {#if error}
        <p class="text-sm text-red-500">{error}</p>
      {/if}
    </div>

    <footer class="px-5 py-3 border-t border-surface-200 dark:border-surface-700 flex items-center gap-2">
      <button
        class="btn preset-filled-primary-500"
        disabled={creating || !roomName.trim()}
        onclick={create}
      >
        {creating ? 'Creating…' : 'Create room'}
      </button>
      <div class="flex-1"></div>
      <button class="btn preset-outlined-surface-500" disabled={creating} onclick={onclose}>
        Cancel
      </button>
    </footer>
  </div>
</div>
