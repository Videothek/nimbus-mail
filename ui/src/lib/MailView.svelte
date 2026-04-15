<script lang="ts">
  /**
   * MailView — the right-hand reading pane.
   *
   * Given an account + folder + UID, calls `fetch_message` to pull the
   * full message (headers + body) from the IMAP server. Renders plain
   * text if we have it, otherwise falls back to the HTML body inside a
   * sandboxed iframe (to keep any remote-content / scripts in the mail
   * isolated from the app).
   */

  import { invoke } from '@tauri-apps/api/core'
  import { formatError } from './errors'

  interface Email {
    id: string
    account_id: string
    folder: string
    from: string
    to: string[]
    cc: string[]
    subject: string
    body_text: string | null
    body_html: string | null
    date: string
    is_read: boolean
    is_starred: boolean
    has_attachments: boolean
  }

  interface Props {
    accountId: string
    folder?: string
    uid: number | null
  }
  let { accountId, folder = 'INBOX', uid }: Props = $props()

  let email = $state<Email | null>(null)
  let loading = $state(false)
  let refreshing = $state(false)
  let error = $state('')

  $effect(() => {
    if (uid == null) {
      email = null
      return
    }
    void load(accountId, folder, uid)
  })

  async function load(id: string, f: string, u: number) {
    loading = true
    refreshing = false
    error = ''
    email = null

    // Cache first — lets the reading pane paint instantly when the user
    // re-opens a previously read message (the common case).
    try {
      const cached = await invoke<Email | null>('get_cached_message', {
        accountId: id,
        folder: f,
        uid: u,
      })
      if (id === accountId && f === folder && u === uid && cached) {
        email = cached
        loading = false
      }
    } catch (e: any) {
      console.warn('get_cached_message failed:', e)
    }

    // Network refresh: pulls fresh flags / body in case the message
    // changed on the server (marked read elsewhere, updated draft, etc.).
    refreshing = email != null
    try {
      const fresh = await invoke<Email>('fetch_message', {
        accountId: id,
        folder: f,
        uid: u,
      })
      if (id === accountId && f === folder && u === uid) {
        email = fresh
      }
    } catch (e: any) {
      if (email == null) {
        error = formatError(e) || 'Failed to load message'
      } else {
        console.warn('fetch_message failed (showing cached):', e)
      }
    } finally {
      loading = false
      refreshing = false
    }
  }

  function formatFullDate(iso: string): string {
    return new Date(iso).toLocaleString()
  }
</script>

<main class="flex-1 flex flex-col overflow-hidden">
  {#if uid == null}
    <div class="flex-1 flex items-center justify-center text-surface-500">
      Select a message to read.
    </div>
  {:else if loading}
    <div class="flex-1 flex items-center justify-center text-surface-500">Loading message…</div>
  {:else if error}
    <div class="p-6 text-sm text-red-500">{error}</div>
  {:else if email}
    <!-- Email header -->
    <div class="p-6 border-b border-surface-200 dark:border-surface-700">
      <div class="flex items-start justify-between mb-2 gap-4">
        <h2 class="text-xl font-semibold">{email.subject || '(no subject)'}</h2>
        <div class="flex items-center gap-3 shrink-0">
          {#if refreshing}
            <span class="text-xs text-surface-500">Refreshing…</span>
          {/if}
          <span class="text-sm text-surface-500">{formatFullDate(email.date)}</span>
        </div>
      </div>
      <div class="flex items-center gap-2 text-sm text-surface-600 dark:text-surface-400">
        <span class="font-medium">{email.from || '(unknown sender)'}</span>
      </div>
      {#if email.to.length > 0}
        <div class="text-xs text-surface-500 mt-1">
          To: {email.to.join(', ')}
        </div>
      {/if}
      {#if email.cc.length > 0}
        <div class="text-xs text-surface-500">
          Cc: {email.cc.join(', ')}
        </div>
      {/if}
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
      {#if email.body_text}
        <!-- Prefer plain text: safe, simple, no remote content. -->
        <pre class="whitespace-pre-wrap font-sans text-sm">{email.body_text}</pre>
      {:else if email.body_html}
        <!--
          HTML-only messages go in a sandboxed iframe. `sandbox=""` (no
          allow-* tokens) disables scripts, form submission, same-origin,
          and top-navigation — so even malicious mail can't attack the app.
        -->
        <iframe
          title="Message body"
          class="w-full h-full border-0 bg-white"
          sandbox=""
          srcdoc={email.body_html}
        ></iframe>
      {:else}
        <p class="text-sm text-surface-500">(This message has no visible body.)</p>
      {/if}
    </div>
  {/if}
</main>
