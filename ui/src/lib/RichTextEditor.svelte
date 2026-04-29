<script lang="ts" module>
  // Module-scope cache for the system font list (#142).  Shared
  // across every RichTextEditor instance in the session so
  // re-opening compose doesn't re-pay the IPC cost.  Filled by
  // the first instance that resolves `list_system_fonts`; the
  // backend cache itself is warmed at app startup, so this ends
  // up being a single ~1ms IPC for the lifetime of the app.
  let systemFontsCache = $state<string[]>([])
</script>

<script lang="ts">
  /**
   * RichTextEditor — Tiptap-based WYSIWYG editor for composing emails.
   *
   * Provides an Outlook-style toolbar with formatting controls: text
   * styles (bold, italic, underline, strikethrough), headings, lists,
   * alignment, links, tables, colors, horizontal rules, and images.
   *
   * The editor exposes its HTML content reactively via `onchange` so the
   * parent (Compose) can read it at send time.
   */

  import { onDestroy } from 'svelte'
  import { createEditor, EditorContent } from 'svelte-tiptap'
  import { mergeAttributes } from '@tiptap/core'
  import StarterKit from '@tiptap/starter-kit'
  import Underline from '@tiptap/extension-underline'
  import Link from '@tiptap/extension-link'
  import Image from '@tiptap/extension-image'
  import TextAlign from '@tiptap/extension-text-align'
  import Placeholder from '@tiptap/extension-placeholder'
  import { TextStyle } from '@tiptap/extension-text-style'
  import { FontFamily } from '@tiptap/extension-font-family'
  import Color from '@tiptap/extension-color'
  import Highlight from '@tiptap/extension-highlight'
  import { Table } from '@tiptap/extension-table'
  import TableRow from '@tiptap/extension-table-row'
  import TableCell from '@tiptap/extension-table-cell'
  import TableHeader from '@tiptap/extension-table-header'
  import Mention from '@tiptap/extension-mention'
  import type { Range } from '@tiptap/core'
  import EmojiPicker from './EmojiPicker.svelte'
  import { invoke } from '@tauri-apps/api/core'

  /**
   * Imperative handle the parent gets via `onready`. Tiptap is
   * stateful and managed inside this component, so the parent can't
   * just reassign `content` and expect the editor to follow. Instead
   * we hand back targeted operations the parent might need.
   */
  export interface EditorApi {
    /** Append raw HTML to the end of the document. Used by Compose
     *  to drop in Nextcloud share links without disturbing the user's
     *  cursor or undo history. */
    appendHtml: (html: string) => void
    /** Replace the entire document with the given HTML. Used by
     *  Compose to swap the active signature when the user picks a
     *  different From: account — caller is responsible for passing
     *  the *full* new body, not a diff. */
    setHtml: (html: string) => void
    /** Insert an image at the current selection. Used by the "insert
     *  from Nextcloud" flow: the parent opens the file picker,
     *  downloads bytes, converts to a data URL, and calls this. */
    insertImage: (src: string) => void
  }

  interface Props {
    /** Initial HTML content (e.g. quoted reply body). */
    content?: string
    /** Placeholder text shown when the editor is empty. */
    placeholder?: string
    /** Fires on every content change with the current HTML. */
    onchange?: (html: string) => void
    /** Fires once the editor instance is ready, handing over a small
     *  imperative API for operations the parent can't drive via props. */
    onready?: (api: EditorApi) => void
    /** If set, the "insert image from Nextcloud" toolbar button calls
     *  this instead of prompting for a URL. The parent is expected to
     *  mount the `NextcloudFilePicker` and hand the picked image's
     *  data URL back via `editorApi.insertImage`. When not provided
     *  the button falls back to the plain URL prompt so embedders
     *  without Nextcloud (tests, future standalone usage) still work. */
    onrequestncimage?: () => void
    /** Async query for the `@` contact picker. Returns two parallel
     *  lists — `participants` (currently in To/Cc/Bcc) shown above the
     *  divider, `others` (rest of the address book matching `query`)
     *  below. The popup wires the keyboard / click → `oncontactpicked`. */
    oncontactquery?: (query: string) => Promise<{
      participants: ContactSuggestion[]
      others: ContactSuggestion[]
    }>
    /** Fires after a `@` contact mention has been inserted. Compose
     *  uses this to add the contact to Cc when the email isn't
     *  already on To/Cc/Bcc — keeps the recipient list and the
     *  body's mentions in sync. */
    oncontactpicked?: (contact: ContactSuggestion) => void
    /** Live attachment list for the `/` reference picker. Each entry
     *  needs `content_id` (the cid: target) and `filename`. The
     *  editor reads this snapshot at every keystroke of the picker —
     *  no separate event needed when the parent's attachments
     *  change. */
    attachmentsForRef?: AttachmentRef[]
    /** Caller-provided actions appended to the right side of the
     *  toolbar (#103).  Compose uses this to colocate the Save /
     *  Discard / Send buttons with the rich-text controls so the
     *  user has one toolbar instead of a top-row + bottom-footer
     *  split.  When omitted the tab strip ends at the trailing
     *  divider, which is what every future embedder without send-
     *  style actions gets by default. */
    actionsTrailing?: import('svelte').Snippet
    /** Extra tabs the embedder wants to add to the toolbar (#103
     *  follow-up).  Compose contributes a single "Attach" tab so
     *  Attach / NC Files / Talk / Event live in the same ribbon as
     *  the rich-text controls.  Each entry is `{ id, label, icon,
     *  content }` — `content` is rendered as the panel below the
     *  tab strip when the tab is active.  Empty / omitted → no
     *  extra tabs. */
    extraTabs?: ExtraTab[]
  }

  /** Embedder-provided tab spec.  Mirrors the ribbon tabs the
   *  editor renders by default but lets a parent like Compose add
   *  Compose-only actions (Attach / Files / Talk / Event) into the
   *  same tab strip as Format / Insert / Layout. */
  export interface ExtraTab {
    /** Stable id used as the `activeTab` value when this tab is
     *  selected.  Avoid collisions with the built-in tab ids
     *  (`'format' | 'insert' | 'layout'`). */
    id: string
    /** Tab strip label, e.g. "Attach". */
    label: string
    /** Optional emoji / icon shown left of the label in the strip. */
    icon?: string
    /** Panel contents — rendered below the tab strip when this tab
     *  is the active one. */
    content: import('svelte').Snippet
  }

  /** A row in the `@` contact picker. */
  export interface ContactSuggestion {
    /** Stable key — typically the email. */
    id: string
    /** Display name shown in the chip and the popup. */
    label: string
    /** Email address used in the mailto: href and plain-text
     *  serialization. */
    email: string
    /** Optional avatar URL (e.g. `convertFileSrc(id, 'contact-photo')`). */
    photoUrl?: string | null
    /** Optional secondary line in the popup row (e.g. organization). */
    hint?: string | null
  }

  /** A row in the `/` attachment picker. */
  export interface AttachmentRef {
    /** RFC 2392 Content-ID, used as the `cid:` target on the
     *  inserted link. Stamped at attachment-pick time in Compose. */
    content_id: string
    filename: string
  }
  let {
    content = '',
    placeholder = 'Write your message\u2026',
    onchange,
    onready,
    onrequestncimage,
    oncontactquery,
    oncontactpicked,
    attachmentsForRef = [],
    actionsTrailing,
    extraTabs = [],
  }: Props = $props()

  // \u2500\u2500 Inline `@` and `/` picker state \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
  // Tiptap's suggestion plugin owns the trigger detection and
  // emits lifecycle hooks; we mirror the relevant bits into Svelte
  // state so the popup renders declaratively below the editor.
  // Two independent slots so `@` and `/` can't both be open at once
  // wins out by sharing `pickerKey` \u2014 only one is mounted at a time.
  interface PickerPosition {
    left: number
    top: number
  }
  interface MentionPickerState {
    visible: boolean
    items: ContactSuggestion[]
    /** Number of `participants` items at the head of `items`. The
     *  popup draws a divider after `participantsCount - 1` so the
     *  user can tell "already on this mail" from "rest of the
     *  address book". Zero = no divider. */
    participantsCount: number
    selectedIndex: number
    position: PickerPosition
    command: ((c: ContactSuggestion) => void) | null
  }
  let mentionPicker = $state<MentionPickerState>({
    visible: false,
    items: [],
    participantsCount: 0,
    selectedIndex: 0,
    position: { left: 0, top: 0 },
    command: null,
  })

  interface AttachmentPickerState {
    visible: boolean
    items: AttachmentRef[]
    selectedIndex: number
    position: PickerPosition
    command: ((a: AttachmentRef) => void) | null
  }
  let attachmentPicker = $state<AttachmentPickerState>({
    visible: false,
    items: [],
    selectedIndex: 0,
    position: { left: 0, top: 0 },
    command: null,
  })

  /** Compute the popup anchor from the trigger char's bounding
   *  rect. Clamped to the viewport so a `@` typed near the right
   *  edge of the modal doesn't push the popup off-screen. */
  function anchorBelow(rect: DOMRect | null | undefined): PickerPosition {
    if (!rect) return { left: 8, top: 8 }
    const gap = 4
    return {
      left: Math.max(8, Math.min(rect.left, window.innerWidth - 280)),
      top: rect.bottom + gap,
    }
  }

  /** Forward editor keystrokes to the visible picker (arrows /
   *  enter / tab / escape) and return whether we consumed them.
   *  Tiptap suggestion uses the boolean to decide whether to let
   *  the keystroke fall through to the editor. Each picker has
   *  its own handler because they own different state slots, but
   *  both share the same key-mapping. */
  function handleMentionKey(event: KeyboardEvent): boolean {
    const len = mentionPicker.items.length
    if (len === 0) return event.key === 'Escape'
    if (event.key === 'ArrowDown') {
      mentionPicker.selectedIndex = (mentionPicker.selectedIndex + 1) % len
      return true
    }
    if (event.key === 'ArrowUp') {
      mentionPicker.selectedIndex =
        (mentionPicker.selectedIndex - 1 + len) % len
      return true
    }
    if (event.key === 'Enter' || event.key === 'Tab') {
      const item = mentionPicker.items[mentionPicker.selectedIndex]
      if (item && mentionPicker.command) mentionPicker.command(item)
      return true
    }
    return event.key === 'Escape'
  }
  function handleAttachmentKey(event: KeyboardEvent): boolean {
    const len = attachmentPicker.items.length
    if (len === 0) return event.key === 'Escape'
    if (event.key === 'ArrowDown') {
      attachmentPicker.selectedIndex = (attachmentPicker.selectedIndex + 1) % len
      return true
    }
    if (event.key === 'ArrowUp') {
      attachmentPicker.selectedIndex =
        (attachmentPicker.selectedIndex - 1 + len) % len
      return true
    }
    if (event.key === 'Enter' || event.key === 'Tab') {
      const item = attachmentPicker.items[attachmentPicker.selectedIndex]
      if (item && attachmentPicker.command) attachmentPicker.command(item)
      return true
    }
    return event.key === 'Escape'
  }

  // svelte-ignore state_referenced_locally
  const editor = createEditor({
    extensions: [
      StarterKit.configure({
        heading: { levels: [1, 2, 3] },
        // Tiptap 3 renamed `History` to `UndoRedo`. `newGroupDelay`
        // groups consecutive keystrokes inside a 500ms window into
        // one undo unit — Ctrl-Z then undoes a word (not a single
        // character), which is what every modern editor does.
        undoRedo: { newGroupDelay: 500 },
      }),
      Underline,
      // Extend the Link mark so every rendered <a> carries a `title`
      // attribute equal to its href. Browsers show `title` as a native
      // tooltip on hover, which lets the user preview where a link
      // actually leads — important both for reviewing what we just
      // pasted and for spotting phishing-style "click here" anchors
      // whose visible text differs from the real destination.
      Link.extend({
        renderHTML({ HTMLAttributes }) {
          const href = (HTMLAttributes as { href?: string }).href
          return [
            'a',
            mergeAttributes(
              this.options.HTMLAttributes,
              HTMLAttributes,
              href ? { title: href } : {},
            ),
            0,
          ]
        },
      }).configure({
        openOnClick: false,
        HTMLAttributes: { target: '_blank', rel: 'noopener noreferrer' },
      }),
      // Image extended with drag-to-resize. A plain `<img>` doesn't
      // expose a native resize affordance, so we:
      //   1. add optional `width` / `height` attributes that parse
      //      from and render into the HTML, so sizes round-trip
      //      through save/open/send,
      //   2. plug in a NodeView that renders a wrapper span around
      //      the `<img>` with a small bottom-right corner handle;
      //      dragging the handle resizes the image in real time
      //      and commits the final width as an attr on pointer-up.
      // The NodeView is plain DOM (no Svelte NodeView wrapper) so
      // it stays under 80 lines and doesn't drag Svelte runtime
      // into ProseMirror's view layer.
      Image.extend({
        addAttributes() {
          return {
            ...this.parent?.(),
            width: {
              default: null,
              parseHTML: (el) => {
                const w = el.getAttribute('width')
                if (w && /^\d+$/.test(w)) return parseInt(w, 10)
                return null
              },
              renderHTML: (attrs) =>
                attrs.width ? { width: String(attrs.width) } : {},
            },
            height: {
              default: null,
              parseHTML: (el) => {
                const h = el.getAttribute('height')
                if (h && /^\d+$/.test(h)) return parseInt(h, 10)
                return null
              },
              renderHTML: (attrs) =>
                attrs.height ? { height: String(attrs.height) } : {},
            },
          }
        },
        addNodeView() {
          return ({ node, editor: ed, getPos }) => {
            const wrap = document.createElement('span')
            wrap.className = 'ev-resizable-img'
            wrap.style.display = 'inline-block'
            wrap.style.position = 'relative'
            wrap.style.maxWidth = '100%'

            const img = document.createElement('img')
            img.src = node.attrs.src
            img.alt = node.attrs.alt ?? ''
            if (node.attrs.width) img.style.width = `${node.attrs.width}px`
            img.style.maxWidth = '100%'
            img.style.height = 'auto'
            img.style.display = 'block'
            wrap.appendChild(img)

            const handle = document.createElement('span')
            handle.className = 'ev-resize-handle'
            handle.setAttribute('aria-hidden', 'true')
            wrap.appendChild(handle)

            handle.addEventListener('pointerdown', (e) => {
              e.preventDefault()
              e.stopPropagation()
              const startX = e.clientX
              const startWidth = img.offsetWidth || img.naturalWidth || 200
              let latestWidth = startWidth
              const onMove = (ev: PointerEvent) => {
                latestWidth = Math.max(50, startWidth + ev.clientX - startX)
                img.style.width = `${latestWidth}px`
              }
              const onUp = () => {
                window.removeEventListener('pointermove', onMove)
                window.removeEventListener('pointerup', onUp)
                const pos = typeof getPos === 'function' ? getPos() : null
                if (pos == null) return
                // Commit the final width as an attribute so it
                // round-trips through save/send. `setNodeSelection`
                // puts the cursor on the node so `updateAttributes`
                // lands on the right one.
                ed.chain()
                  .setNodeSelection(pos)
                  .updateAttributes('image', { width: Math.round(latestWidth) })
                  .run()
              }
              window.addEventListener('pointermove', onMove)
              window.addEventListener('pointerup', onUp)
            })

            return {
              dom: wrap,
              update(updatedNode) {
                // Reject updates of a different type so ProseMirror
                // falls back to full re-render; accept same-type
                // updates and re-sync the width.
                if (updatedNode.type.name !== 'image') return false
                img.src = updatedNode.attrs.src
                img.alt = updatedNode.attrs.alt ?? ''
                if (updatedNode.attrs.width) {
                  img.style.width = `${updatedNode.attrs.width}px`
                } else {
                  img.style.width = ''
                }
                return true
              },
            }
          }
        },
      }).configure({ inline: true }),
      TextAlign.configure({ types: ['heading', 'paragraph'] }),
      // svelte-ignore state_referenced_locally
      Placeholder.configure({ placeholder }),
      // TextStyle is the mark that FontFamily / Color attach to;
      // the extensions are cumulative — adding more marks here
      // doesn't invalidate existing content.
      TextStyle,
      FontFamily,
      Color,
      Highlight.configure({ multicolor: true }),
      Table.configure({ resizable: true }),
      TableRow,
      // Extend TableCell with a backgroundColor attribute so the
      // toolbar's cell-colour picker has somewhere to write.  The
      // attr round-trips via inline `style="background-color: …"`,
      // which every mail client renders correctly without needing
      // a class-based stylesheet on the recipient side.
      TableCell.extend({
        addAttributes() {
          return {
            ...this.parent?.(),
            backgroundColor: {
              default: null,
              parseHTML: (el: HTMLElement) =>
                el.style.backgroundColor || null,
              renderHTML: (attrs: Record<string, unknown>) => {
                const c = attrs.backgroundColor
                if (!c) return {}
                return { style: `background-color: ${c}` }
              },
            },
          }
        },
      }),
      TableHeader,
      // ── `@` contact mention ────────────────────────────────
      // Renamed from the default `mention` so it can coexist with
      // the `/` attachment-ref extension below (Tiptap's Mention is
      // a single Node type — to register two we extend twice with
      // different `name`s). Renders to the wire as
      // `<a href="mailto:…" data-contact-mention>@Alice</a>` so
      // non-Tiptap clients see a clickable mailto link, while the
      // `data-` marker lets us re-parse it on draft round-trip.
      Mention.extend({
        name: 'contactMention',
        renderHTML({ node, HTMLAttributes }) {
          const email = node.attrs.id ?? ''
          const label = node.attrs.label ?? email
          return [
            'a',
            { ...HTMLAttributes, href: `mailto:${email}` },
            `@${label}`,
          ]
        },
        // Plain-text serialization: feed `body_text` an RFC-style
        // address rather than just the bare display name so the
        // text-only fallback still tells a recipient who Alice is.
        renderText({ node }) {
          const label = node.attrs.label ?? node.attrs.id ?? ''
          const email = node.attrs.id ?? ''
          if (label && email && label !== email) return `${label} <${email}>`
          return email || label
        },
        parseHTML() {
          return [
            {
              tag: 'a[data-contact-mention]',
              getAttrs: (el) => {
                const href = (el as HTMLElement).getAttribute('href') ?? ''
                const email = href.replace(/^mailto:/, '')
                const text = (el as HTMLElement).textContent ?? ''
                const label = text.replace(/^@/, '') || email
                return { id: email, label }
              },
            },
          ]
        },
      }).configure({
        HTMLAttributes: {
          'data-contact-mention': '',
          class:
            'inline-block px-1 rounded bg-primary-500/15 text-primary-700 dark:text-primary-300 no-underline',
        },
        suggestion: {
          char: '@',
          // `items` is called every time the query changes. We
          // delegate to the parent's `oncontactquery` (Compose hands
          // back the merged participants + address-book list) and
          // stash `participantsCount` on the array so the popup
          // hooks can read it back without changing Tiptap's
          // expected return shape.
          items: async ({ query }) => {
            if (!oncontactquery) return []
            const { participants, others } = await oncontactquery(query)
            const merged = [...participants, ...others]
            ;(merged as unknown as { __pcount: number }).__pcount = participants.length
            return merged
          },
          command: ({ editor, range, props }) => {
            const c = props as ContactSuggestion
            editor
              .chain()
              .focus()
              .insertContentAt(range, [
                { type: 'contactMention', attrs: { id: c.email, label: c.label } },
                { type: 'text', text: ' ' },
              ])
              .run()
            oncontactpicked?.(c)
          },
          render: () => ({
            onStart: (props) => {
              const items = props.items as ContactSuggestion[]
              const pcount =
                (items as unknown as { __pcount?: number }).__pcount ?? 0
              mentionPicker.items = items
              mentionPicker.participantsCount = pcount
              mentionPicker.selectedIndex = 0
              mentionPicker.command = (c) =>
                (props.command as (data: ContactSuggestion) => void)(c)
              mentionPicker.position = anchorBelow(props.clientRect?.())
              mentionPicker.visible = true
            },
            onUpdate: (props) => {
              const items = props.items as ContactSuggestion[]
              const pcount =
                (items as unknown as { __pcount?: number }).__pcount ?? 0
              mentionPicker.items = items
              mentionPicker.participantsCount = pcount
              mentionPicker.selectedIndex = 0
              mentionPicker.command = (c) =>
                (props.command as (data: ContactSuggestion) => void)(c)
              mentionPicker.position = anchorBelow(props.clientRect?.())
            },
            onKeyDown: ({ event }) => handleMentionKey(event),
            onExit: () => {
              mentionPicker.visible = false
              mentionPicker.items = []
              mentionPicker.command = null
            },
          }),
        },
      }),

      // ── `/` attachment reference ───────────────────────────
      // Same Mention machinery, different node + char + render. The
      // inserted node is a clickable `cid:` link — recipients on
      // Nimbus (or any client that resolves `cid:` href) get a
      // direct jump to the attachment; on Gmail / web clients it
      // falls back to plain link text with the attachment still
      // visible in the message's attachment tray.
      Mention.extend({
        name: 'attachmentRef',
        renderHTML({ node, HTMLAttributes }) {
          const cid = node.attrs.id ?? ''
          const label = node.attrs.label ?? cid
          return [
            'a',
            { ...HTMLAttributes, href: `cid:${cid}` },
            `📎 ${label}`,
          ]
        },
        renderText({ node }) {
          // Plain-text fallback is just the filename — `cid:` URIs
          // mean nothing to a human reading the text/plain part.
          return node.attrs.label ?? ''
        },
        parseHTML() {
          return [
            {
              tag: 'a[data-attachment-ref]',
              getAttrs: (el) => {
                const href = (el as HTMLElement).getAttribute('href') ?? ''
                const id = href.replace(/^cid:/, '')
                const text = (el as HTMLElement).textContent ?? ''
                const label = text.replace(/^📎\s*/, '') || id
                return { id, label }
              },
            },
          ]
        },
      }).configure({
        HTMLAttributes: {
          'data-attachment-ref': '',
          class: 'inline-block text-primary-600 dark:text-primary-400 underline',
        },
        suggestion: {
          char: '/',
          items: ({ query }) => {
            const q = query.trim().toLowerCase()
            return attachmentsForRef
              .filter((a) => a.content_id)
              .filter((a) => !q || a.filename.toLowerCase().includes(q))
              .slice(0, 8)
          },
          command: ({ editor, range, props }) => {
            // Tiptap types `props` as MentionNodeAttrs; we always
            // pass our own AttachmentRef shape from `items` above,
            // so the unknown-cast is the standard escape hatch.
            const a = props as unknown as AttachmentRef
            editor
              .chain()
              .focus()
              .insertContentAt(range, [
                { type: 'attachmentRef', attrs: { id: a.content_id, label: a.filename } },
                { type: 'text', text: ' ' },
              ])
              .run()
          },
          render: () => ({
            onStart: (props) => {
              attachmentPicker.items = props.items as unknown as AttachmentRef[]
              attachmentPicker.selectedIndex = 0
              attachmentPicker.command = props.command as unknown as (
                data: AttachmentRef,
              ) => void
              attachmentPicker.position = anchorBelow(props.clientRect?.())
              attachmentPicker.visible = true
            },
            onUpdate: (props) => {
              attachmentPicker.items = props.items as unknown as AttachmentRef[]
              attachmentPicker.selectedIndex = 0
              attachmentPicker.command = props.command as unknown as (
                data: AttachmentRef,
              ) => void
              attachmentPicker.position = anchorBelow(props.clientRect?.())
            },
            onKeyDown: ({ event }) => handleAttachmentKey(event),
            onExit: () => {
              attachmentPicker.visible = false
              attachmentPicker.items = []
              attachmentPicker.command = null
            },
          }),
        },
      }),
    ],
    // svelte-ignore state_referenced_locally
    content,
    onUpdate: ({ editor: e }) => {
      onchange?.(e.getHTML())
    },
  })

  onDestroy(() => {
    $editor?.destroy()
  })

  // Hand the parent a small imperative API once the editor is live.
  // Tiptap's createEditor returns a Readable store that publishes the
  // instance asynchronously (after the DOM mounts), so we wait for the
  // first non-null value before firing onready.
  $effect(() => {
    const ed = $editor
    if (ed && onready) {
      onready({
        appendHtml: (html: string) => {
          // `insertContentAt(end)` keeps the user's selection where it
          // is — appending a paragraph at the document end is the
          // expected gesture for "append" rather than "insert here".
          ed.chain().insertContentAt(ed.state.doc.content.size, html).run()
        },
        setHtml: (html: string) => {
          // `emitUpdate: false` skips the `onUpdate` callback — the
          // caller already knows the new content (they passed it) and
          // we don't want to round-trip back through `onchange` and
          // re-trigger reactive effects watching `bodyHtml`.
          ed.commands.setContent(html, { emitUpdate: false })
        },
        insertImage: (src: string) => {
          // Run through `chain().focus()` so the editor takes focus
          // even if the insert was triggered from a modal in the
          // parent — otherwise Tiptap's selection can sit outside
          // the document and the image lands in the wrong place.
          ed.chain().focus().setImage({ src }).run()
        },
      })
    }
  })

  // ── Toolbar helpers ─────────────────────────────────────────

  // ── Toolbar helpers ─────────────────────────────────────────
  // Each helper grabs the editor from the store at call time (not
  // capture time) so it's always the live instance.

  function cmd() {
    return $editor!.chain().focus()
  }

  function toggleHeading(level: 1 | 2 | 3) {
    cmd().toggleHeading({ level }).run()
  }

  function doUndo() { cmd().undo().run() }
  function doRedo() { cmd().redo().run() }

  function setLink() {
    const prev = $editor?.getAttributes('link')?.href ?? ''
    const url = window.prompt('URL', prev)
    if (url === null) return
    if (url === '') {
      cmd().extendMarkRange('link').unsetLink().run()
    } else {
      cmd().extendMarkRange('link').setLink({ href: url }).run()
    }
  }

  /** Insert an image from a local file (embedded as data URL). */
  function addImageFromFile() {
    const input = document.createElement('input')
    input.type = 'file'
    input.accept = 'image/*'
    input.onchange = () => {
      const file = input.files?.[0]
      if (!file) return
      const reader = new FileReader()
      reader.onload = () => {
        const src = reader.result as string
        cmd().setImage({ src }).run()
      }
      reader.readAsDataURL(file)
    }
    input.click()
  }

  /** Request an image via the parent-supplied picker, or fall back
   *  to a raw URL prompt if the embedder didn't provide one. The
   *  picker path (Compose → NextcloudFilePicker) is what users
   *  actually want — the URL prompt just keeps the component
   *  self-contained for anywhere we reuse it without a Nextcloud
   *  backend. */
  function addImageFromNcOrUrl() {
    if (onrequestncimage) {
      onrequestncimage()
      return
    }
    const url = window.prompt('Image URL')
    if (url) {
      cmd().setImage({ src: url }).run()
    }
  }

  // ── Font family picker ─────────────────────────────────────
  //
  // Families we expose in the toolbar. Each entry is `{label, css}`
  // — label is what the user sees, `css` is the literal
  // `font-family` value Tiptap writes into `<span style="font-
  // family: …">`. Using system-font stacks (not single names)
  // means the recipient's client renders something reasonable
  // even when their OS doesn't have the exact face installed.
  const CURATED_FONTS: Array<{ label: string; css: string }> = [
    { label: 'Default', css: '' },
    {
      label: 'Sans-serif',
      css: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
    },
    { label: 'Serif', css: 'Georgia, "Times New Roman", Times, serif' },
    {
      label: 'Monospace',
      css: '"SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
    },
    { label: 'Arial', css: 'Arial, Helvetica, sans-serif' },
    { label: 'Times', css: '"Times New Roman", Times, serif' },
  ]
  /** OS-installed font families discovered via the
   *  `list_system_fonts` Tauri command (#142).  Loaded once on
   *  mount; failures (sandboxed dev / missing perm) leave the
   *  list empty so the dropdown still shows the curated stacks.
   *  Module-scope cached so multiple compose windows in one
   *  session don't each round-trip to the backend. */
  let systemFonts = $state<string[]>(systemFontsCache)
  $effect(() => {
    if (systemFontsCache.length > 0) {
      systemFonts = systemFontsCache
      return
    }
    void invoke<string[]>('list_system_fonts')
      .then((rows) => {
        const curatedLabels = new Set(
          CURATED_FONTS.map((f) => f.label.toLowerCase()),
        )
        systemFontsCache = rows.filter((f) => !curatedLabels.has(f.toLowerCase()))
        systemFonts = systemFontsCache
      })
      .catch((e) => {
        console.warn('list_system_fonts failed', e)
      })
  })
  /** Quote a single family name for safe insertion into a CSS
   *  `font-family` string.  Names that contain spaces or non-
   *  word characters need surrounding quotes per CSS3. */
  function familyToCss(name: string): string {
    if (/^[\w-]+$/.test(name)) return name
    return `"${name.replace(/"/g, '\\"')}"`
  }
  /** Combined picker rows — curated stacks at the top, then the
   *  user's OS fonts filtered by the in-picker search box. */
  const FONT_FAMILIES = $derived.by(() => {
    const q = fontPickerQuery.trim().toLowerCase()
    const curated = q
      ? CURATED_FONTS.filter((f) => f.label.toLowerCase().includes(q))
      : CURATED_FONTS
    const os = (q
      ? systemFonts.filter((f) => f.toLowerCase().includes(q))
      : systemFonts
    ).map((f) => ({ label: f, css: familyToCss(f) }))
    return [...curated, ...os]
  })
  let showFontPicker = $state(false)
  let fontPickerQuery = $state('')
  // ── Font picker windowing (#142 follow-up) ─────────────────
  // Even with content-visibility, Svelte mounts a DOM node per
  // {#each} entry on first show.  ~500 buttons × per-button
  // font-family = perceptible click-to-open lag on the first
  // open after launch.  Window the list manually: render only
  // the rows actually visible plus a small buffer, with a tall
  // spacer reserving the full scroll height.
  const FONT_ROW_H = 28
  const FONT_VIEWPORT_H = 288 // mirrors `max-h-72`
  const FONT_BUFFER = 6
  let fontScrollY = $state(0)
  let fontScrollEl: HTMLDivElement | null = $state(null)
  const fontWindow = $derived.by(() => {
    const total = FONT_FAMILIES.length
    const visibleCount = Math.ceil(FONT_VIEWPORT_H / FONT_ROW_H) + FONT_BUFFER * 2
    const start = Math.max(0, Math.floor(fontScrollY / FONT_ROW_H) - FONT_BUFFER)
    const end = Math.min(total, start + visibleCount)
    return {
      start,
      end,
      total,
      slice: FONT_FAMILIES.slice(start, end),
    }
  })
  // Reset scroll back to 0 whenever the picker opens or the
  // search query changes — otherwise a previous scroll position
  // would carry over into a filtered list whose total height is
  // smaller, leaving the user staring at empty space.
  $effect(() => {
    void fontPickerQuery
    void showFontPicker
    if (fontScrollEl) fontScrollEl.scrollTop = 0
    fontScrollY = 0
  })

  /** Label for the toolbar button, reflecting the font at the current
      cursor position. `$editor` is a svelte-tiptap store that re-emits
      on every editor transaction, so this function re-runs after every
      selection change or edit — the label flips in step with the
      cursor. Falls back to the generic "Font" when the cursor sits in
      text carrying a family we don't have a pretty label for. */
  function currentFontLabel(): string {
    if (!$editor) return 'Font'
    const css = ($editor.getAttributes('textStyle')?.fontFamily as string | undefined) ?? ''
    if (!css) return 'Default'
    const curated = CURATED_FONTS.find((f) => f.css === css)
    if (curated) return curated.label
    const sys = systemFonts.find((f) => familyToCss(f) === css)
    if (sys) return sys
    return 'Font'
  }

  function setFont(css: string) {
    showFontPicker = false
    if (!$editor) return
    const ed = $editor

    // Non-empty selection: apply the mark to the selected range. This
    // has always worked via Tiptap's `setFontFamily` helper and we
    // keep using it so existing behavior (selected text rewrites to
    // the new face) is unchanged.
    if (!ed.state.selection.empty) {
      if (css === '') {
        ed.chain().focus().unsetFontFamily().run()
      } else {
        ed.chain().focus().setFontFamily(css).run()
      }
      return
    }

    // Empty selection: we want the *next* typed characters to use the
    // picked font. Tiptap's `setFontFamily` delegates to ProseMirror's
    // `setMark`, which on an empty selection adds to `storedMarks` —
    // in theory that's enough. In practice, because the toolbar
    // button steals focus on click and the click-to-editor focus
    // handoff produces an extra selection transaction, the stored
    // mark was getting cleared before the user's next keystroke.
    //
    // Dispatching `addStoredMark` as a standalone transaction —
    // *after* we explicitly return focus to the editor — sidesteps
    // the focus-handoff race: by the time this tr lands, the editor
    // owns focus and the selection is stable, so the mark stays
    // attached to the cursor position and rides along with the next
    // character the user types.
    ed.commands.focus()
    const { state, view } = ed
    const markType = state.schema.marks.textStyle
    if (!markType) return
    let tr = state.tr
    tr = css === ''
      ? tr.removeStoredMark(markType)
      : tr.addStoredMark(markType.create({ fontFamily: css }))
    view.dispatch(tr)
  }

  // ── Table grid picker state ────────────────────────────────
  let showTablePicker = $state(false)
  let tableHoverRows = $state(0)
  let tableHoverCols = $state(0)
  const TABLE_GRID = 8  // 8x8 picker like Outlook

  /** Selection (cursor position) snapshotted when the user opens
   *  the table picker.  We restore it before inserting so the
   *  table lands where the user's cursor was, not wherever Tiptap
   *  thinks the focus moved to during the dropdown interaction. */
  let savedTableSelection: { from: number; to: number } | null = null

  function openTablePicker() {
    if ($editor) {
      const sel = $editor.state.selection
      savedTableSelection = { from: sel.from, to: sel.to }
    }
    showTablePicker = !showTablePicker
  }

  function insertTable(rows: number, cols: number) {
    if (savedTableSelection) {
      cmd()
        .setTextSelection(savedTableSelection)
        .insertTable({ rows, cols, withHeaderRow: true })
        .run()
    } else {
      cmd().insertTable({ rows, cols, withHeaderRow: true }).run()
    }
    savedTableSelection = null
    showTablePicker = false
  }

  // ── Table editing actions (#103 follow-up) ─────────────────────
  // Thin wrappers around Tiptap's table commands.  The toolbar
  // disables them when the cursor isn't inside a table, so calling
  // them in that state would no-op anyway — but keeping the chain
  // explicit means future "is the cursor in a header row?" checks
  // can branch here without each call site reimplementing it.
  function tblAddRowAbove() { cmd().addRowBefore().run() }
  function tblAddRowBelow() { cmd().addRowAfter().run() }
  function tblAddColLeft()  { cmd().addColumnBefore().run() }
  function tblAddColRight() { cmd().addColumnAfter().run() }
  function tblDeleteRow()   { cmd().deleteRow().run() }
  function tblDeleteCol()   { cmd().deleteColumn().run() }
  function tblDelete()      { cmd().deleteTable().run() }
  function tblSetCellColor(e: Event) {
    const color = (e.target as HTMLInputElement).value
    cmd().setCellAttribute('backgroundColor', color).run()
  }
  function tblClearCellColor() {
    cmd().setCellAttribute('backgroundColor', null).run()
  }

  /** Reactive: is the cursor currently inside a table?  Drives the
   *  enabled/disabled state of the table-edit buttons in the
   *  Insert tab. */
  function tableActive(): boolean {
    return !!$editor?.isActive('table')
  }

  // ── Ribbon tab + emoji-picker state (#103 follow-up) ──────────
  // The toolbar is split into Outlook-style tabs.  `activeTab`
  // drives which panel is rendered below the tab strip; values
  // beyond the built-in three come from the embedder's
  // `extraTabs` prop (Compose contributes "attach").
  type BuiltinTab = 'format' | 'insert' | 'layout'
  let activeTab = $state<BuiltinTab | string>('format')

  let showEmojiPicker = $state(false)

  function insertEmoji(e: string | null) {
    if (!e) return
    cmd().insertContent(e).run()
    showEmojiPicker = false
  }

  // Click-outside dismissal for the emoji popup.  The popup itself
  // stops propagation on its own click handler, so any click that
  // reaches `document` originated outside it.  We delay the install
  // by one tick (`setTimeout(0)`) so the click that *opened* the
  // popup doesn't immediately close it.  Same idiom would work for
  // the other popups (font / table) but the user only flagged the
  // emoji picker — keeping scope tight.
  $effect(() => {
    if (!showEmojiPicker) return
    const close = () => (showEmojiPicker = false)
    const handle = setTimeout(() => {
      window.addEventListener('click', close)
    }, 0)
    return () => {
      clearTimeout(handle)
      window.removeEventListener('click', close)
    }
  })

  /** Strip every mark + collapse the current block down to the
   *  default paragraph node.  Outlook's "Clear formatting" button. */
  function clearFormatting() {
    cmd().unsetAllMarks().clearNodes().run()
  }

  function setColor(e: Event) {
    const color = (e.target as HTMLInputElement).value
    cmd().setColor(color).run()
  }

  function setHighlight(e: Event) {
    const color = (e.target as HTMLInputElement).value
    cmd().toggleHighlight({ color }).run()
  }

  // Reactive "is active" helpers for styling toolbar buttons.
  // Returns the `is-active` class which `.rt-btn` (panel buttons)
  // and `.tb` (compact buttons) both pick up via `:global` rules.
  const ACTIVE_CLS = 'is-active'

  function active(name: string, attrs?: Record<string, unknown>): string {
    return $editor?.isActive(name, attrs) ? ACTIVE_CLS : ''
  }

  function activeAttrs(attrs: Record<string, unknown>): string {
    return $editor?.isActive(attrs) ? ACTIVE_CLS : ''
  }
</script>

<style>
  /* Compact toolbar buttons — used by the tab strip's undo/redo +
     embedder-supplied trailing actions where vertical space is
     scarce.  Same idiom as before; the ribbon's panel buttons get
     `.rt-btn` styling instead. */
  .tb {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0.25rem 0.5rem;
    border-radius: 0.25rem;
    font-size: 0.75rem;
    line-height: 1;
    cursor: pointer;
    transition: background 0.1s;
    position: relative;
    border: none;
    background: transparent;
    color: inherit;
  }
  .tb:hover {
    background: var(--color-surface-200);
  }
  :global(.dark) .tb:hover {
    background: var(--color-surface-700);
  }
  .tb.is-active {
    background: var(--color-surface-300);
  }
  :global(.dark) .tb.is-active {
    background: var(--color-surface-600);
  }

  /* ── Ribbon-style tab strip (#103 follow-up) ─────────────────── */

  /* Tab buttons.  Rounded-top chip with a primary-colour underline
     when active, matching Outlook Web's ribbon tabs. */
  :global(.rt-tab) {
    padding: 0.45rem 1rem;
    font-size: 0.8125rem;
    font-weight: 500;
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
    border-bottom: 2px solid transparent;
    border-top-left-radius: 0.25rem;
    border-top-right-radius: 0.25rem;
    line-height: 1;
    transition: background 0.1s, border-color 0.1s, color 0.1s;
  }
  :global(.rt-tab:hover) {
    background: var(--color-surface-200);
  }
  :global([data-mode='dark'] .rt-tab:hover) {
    background: var(--color-surface-700);
  }
  :global(.rt-tab-active) {
    color: var(--color-primary-500);
    border-bottom-color: var(--color-primary-500);
  }

  /* Panel buttons — large stacked icon-above-label.  Outlook-Web
     ribbon proportions: ~50px tall, 24px icon, 11px label. */
  :global(.rt-btn) {
    display: inline-flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.125rem;
    min-width: 3.25rem;
    padding: 0.375rem 0.5rem;
    border-radius: 0.375rem;
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
    transition: background 0.1s, color 0.1s;
    position: relative;
  }
  :global(.rt-btn:hover:not(:disabled)) {
    background: var(--color-surface-200);
  }
  :global([data-mode='dark'] .rt-btn:hover:not(:disabled)) {
    background: var(--color-surface-700);
  }
  :global(.rt-btn:disabled) {
    opacity: 0.4;
    cursor: not-allowed;
  }
  :global(.rt-btn-icon) {
    font-size: 1.125rem;
    line-height: 1;
    height: 1.25rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  :global(.rt-btn-label) {
    font-size: 0.6875rem;
    line-height: 1;
    white-space: nowrap;
  }
  :global(.rt-btn-wide) {
    min-width: 6rem;
  }
  :global(.rt-btn.is-active) {
    background: rgb(from var(--color-primary-500) r g b / 0.15);
    color: var(--color-primary-500);
  }

  /* Vertical rule between sub-groups inside a panel. */
  :global(.rt-divider) {
    display: inline-block;
    width: 1px;
    height: 2.25rem;
    background: var(--color-surface-300);
    margin: 0 0.375rem;
    align-self: center;
  }
  :global([data-mode='dark'] .rt-divider) {
    background: var(--color-surface-600);
  }
  /* Tiptap editor chrome — keep the editing area clean and consistent
     with the rest of the app. */
  :global(.tiptap) {
    outline: none;
    min-height: 200px;
    padding: 0.75rem;
    font-size: 0.875rem;
    line-height: 1.625;
  }
  :global(.tiptap p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    pointer-events: none;
    height: 0;
    color: var(--color-surface-400);
  }
  /* Basic table styling so it's visible in the editor. */
  :global(.tiptap table) {
    border-collapse: collapse;
    width: 100%;
    margin: 0.5rem 0;
  }
  :global(.tiptap th),
  :global(.tiptap td) {
    border: 1px solid var(--color-surface-300);
    padding: 0.375rem 0.625rem;
    text-align: left;
    min-width: 80px;
  }
  :global([data-mode='dark'] .tiptap th),
  :global([data-mode='dark'] .tiptap td) {
    border-color: var(--color-surface-700);
  }
  :global(.tiptap th) {
    background: var(--color-surface-100);
    font-weight: 600;
  }
  :global([data-mode='dark'] .tiptap th) {
    background: var(--color-surface-800);
  }
  :global(.tiptap img) {
    max-width: 100%;
    height: auto;
  }
  :global(.tiptap blockquote) {
    border-left: 3px solid var(--color-surface-300);
    padding-left: 0.75rem;
    margin: 0.5rem 0;
    color: var(--color-surface-600);
  }
  :global([data-mode='dark'] .tiptap blockquote) {
    border-left-color: var(--color-surface-700);
    color: var(--color-surface-400);
  }
  :global(.tiptap hr) {
    border: none;
    border-top: 1px solid var(--color-surface-300);
    margin: 1rem 0;
  }
  :global([data-mode='dark'] .tiptap hr) {
    border-top-color: var(--color-surface-700);
  }
  :global(.tiptap ul),
  :global(.tiptap ol) {
    padding-left: 1.5rem;
    margin: 0.25rem 0;
  }
  :global(.tiptap ul) { list-style-type: disc; }
  :global(.tiptap ol) { list-style-type: decimal; }
  :global(.tiptap a) { color: var(--color-primary-500); text-decoration: underline; }

  /* Image resize handle styling. Positioned at the bottom-right
     corner of the wrapper span inserted by our NodeView (see
     `addNodeView` on the Image extension). The handle stays
     invisible until the image or wrapper is hovered so the editor
     doesn't show UI chrome on every image on first render. */
  :global(.tiptap .ev-resizable-img) {
    line-height: 0;
  }
  :global(.tiptap .ev-resize-handle) {
    position: absolute;
    right: -4px;
    bottom: -4px;
    width: 12px;
    height: 12px;
    border: 2px solid var(--color-primary-500);
    background: var(--color-surface-50);
    border-radius: 2px;
    cursor: nwse-resize;
    opacity: 0;
    transition: opacity 120ms ease;
    touch-action: none;
  }
  :global(.tiptap .ev-resizable-img:hover .ev-resize-handle) {
    opacity: 1;
  }
</style>

{#if $editor}
<!-- Wrapper: `h-full flex flex-col min-h-0` lets the editor fill whatever
     vertical space its parent gives it (e.g. when the Compose modal is
     resized taller). The toolbar stays at natural height; the content
     area below claims the remaining space via `flex-1`. Works in a
     block parent too — `h-full` is simply ignored and the editor falls
     back to its intrinsic 200 px minimum. -->
<div class="h-full flex flex-col min-h-0">
  <!-- ── Ribbon: tab strip + active panel (#103) ───────────────────
       Outlook-style two-row toolbar.  Top row holds the tab labels
       on the left, undo/redo + the embedder's send actions on the
       right.  Bottom row renders the active tab's panel — bigger
       icon-above-label buttons for a less flat, more discoverable
       look than the previous single-row layout. -->
  <div class="border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800">
    <!-- Tab strip -->
    <div class="flex items-stretch gap-0 px-2 pt-0.5" role="tablist">
      <button
        type="button"
        role="tab"
        aria-selected={activeTab === 'format'}
        class="rt-tab"
        class:rt-tab-active={activeTab === 'format'}
        onclick={() => (activeTab = 'format')}
      >Format</button>
      <button
        type="button"
        role="tab"
        aria-selected={activeTab === 'insert'}
        class="rt-tab"
        class:rt-tab-active={activeTab === 'insert'}
        onclick={() => (activeTab = 'insert')}
      >Insert</button>
      <button
        type="button"
        role="tab"
        aria-selected={activeTab === 'layout'}
        class="rt-tab"
        class:rt-tab-active={activeTab === 'layout'}
        onclick={() => (activeTab = 'layout')}
      >Layout</button>
      {#each extraTabs as t (t.id)}
        <button
          type="button"
          role="tab"
          aria-selected={activeTab === t.id}
          class="rt-tab"
          class:rt-tab-active={activeTab === t.id}
          onclick={() => (activeTab = t.id)}
        >
          {#if t.icon}<span class="mr-1">{t.icon}</span>{/if}{t.label}
        </button>
      {/each}

      <!-- Top-right: undo/redo + caller's send-side actions.  Lives
           in the tab strip rather than inside any panel because the
           user expects Send + Save + Undo to be reachable
           regardless of which tab is open. -->
      <div class="ml-auto flex items-center gap-1 px-1">
        <button class="tb" title="Undo (Ctrl+Z)" onclick={() => doUndo()}>↩</button>
        <button class="tb" title="Redo (Ctrl+Y)" onclick={() => doRedo()}>↪</button>
        {#if actionsTrailing}
          <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>
          {@render actionsTrailing()}
        {/if}
      </div>
    </div>

    <!-- Tab panel — flex row of large stacked-icon buttons.  Each
         tab's content lives behind its own `{#if}` so swapping tabs
         doesn't carry hidden DOM.  Dividers split logical sub-
         groups within a panel for scannability. -->
    <div class="flex flex-wrap items-center gap-0.5 px-2 py-1.5 min-h-[3rem]">
      {#if activeTab === 'format'}
        <!-- Font family — wider trigger, dropdown menu. -->
        <div class="relative inline-block">
          <button
            type="button"
            class="rt-btn rt-btn-wide"
            title="Font family"
            onclick={() => (showFontPicker = !showFontPicker)}
          >
            <span class="rt-btn-icon" aria-hidden="true">𝐀</span>
            <span class="rt-btn-label">{currentFontLabel()} ▾</span>
          </button>
          {#if showFontPicker}
            <div
              class="absolute z-20 mt-1 w-64 rounded-md border border-surface-200 dark:border-surface-700 bg-surface-50 dark:bg-surface-900 shadow-md py-1 flex flex-col"
              role="menu"
              tabindex="-1"
              onclick={(e) => e.stopPropagation()}
              onkeydown={(e) => { if (e.key === 'Escape') { showFontPicker = false; fontPickerQuery = '' } }}
            >
              <div class="px-2 pt-1 pb-2 border-b border-surface-200 dark:border-surface-700">
                <input
                  type="search"
                  class="input w-full text-sm px-2 py-1 rounded-md"
                  placeholder="Search fonts ({systemFonts.length} system)"
                  bind:value={fontPickerQuery}
                />
              </div>
              <!-- Windowed scroll container — absolute-positions
                   only the rows currently in (or close to) the
                   viewport.  Total scroll height is reserved by
                   a single spacer div sized to total*ROW_H so
                   the scrollbar geometry matches an unwindowed
                   list. -->
              <div
                bind:this={fontScrollEl}
                class="max-h-72 overflow-y-auto"
                onscroll={(e) => (fontScrollY = (e.currentTarget as HTMLDivElement).scrollTop)}
              >
                {#if fontWindow.total === 0}
                  <p class="px-3 py-2 text-xs text-surface-500 italic">
                    No fonts match "{fontPickerQuery}".
                  </p>
                {:else}
                  <div style="position: relative; height: {fontWindow.total * FONT_ROW_H}px;">
                    {#each fontWindow.slice as f, i (f.label)}
                      <button
                        type="button"
                        class="absolute left-0 right-0 text-left px-3 text-sm leading-tight hover:bg-surface-200 dark:hover:bg-surface-800 truncate"
                        style="top: {(fontWindow.start + i) * FONT_ROW_H}px; height: {FONT_ROW_H}px;{f.css ? ` font-family: ${f.css};` : ''}"
                        onclick={() => { setFont(f.css); fontPickerQuery = '' }}
                      >{f.label}</button>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>
          {/if}
        </div>

        <span class="rt-divider"></span>

        <!-- Text style -->
        <button class="rt-btn {active('bold')}" title="Bold (Ctrl+B)" onclick={() => $editor?.chain().focus().toggleBold().run()}>
          <span class="rt-btn-icon"><strong>B</strong></span>
          <span class="rt-btn-label">Bold</span>
        </button>
        <button class="rt-btn {active('italic')}" title="Italic (Ctrl+I)" onclick={() => $editor?.chain().focus().toggleItalic().run()}>
          <span class="rt-btn-icon"><em>I</em></span>
          <span class="rt-btn-label">Italic</span>
        </button>
        <button class="rt-btn {active('underline')}" title="Underline (Ctrl+U)" onclick={() => $editor?.chain().focus().toggleUnderline().run()}>
          <span class="rt-btn-icon"><u>U</u></span>
          <span class="rt-btn-label">Underline</span>
        </button>
        <button class="rt-btn {active('strike')}" title="Strikethrough" onclick={() => $editor?.chain().focus().toggleStrike().run()}>
          <span class="rt-btn-icon"><s>S</s></span>
          <span class="rt-btn-label">Strike</span>
        </button>

        <span class="rt-divider"></span>

        <!-- Colors -->
        <label class="rt-btn cursor-pointer" title="Text color">
          <span class="rt-btn-icon" style="border-bottom: 3px solid currentColor;">A</span>
          <span class="rt-btn-label">Color</span>
          <input type="color" class="w-0 h-0 opacity-0 absolute" onchange={setColor} />
        </label>
        <label class="rt-btn cursor-pointer" title="Highlight color">
          <span class="rt-btn-icon"><span class="bg-yellow-200 dark:bg-yellow-300 px-0.5 rounded-sm text-surface-900">H</span></span>
          <span class="rt-btn-label">Highlight</span>
          <input type="color" value="#fde68a" class="w-0 h-0 opacity-0 absolute" onchange={setHighlight} />
        </label>

        <span class="rt-divider"></span>

        <!-- Clear formatting — strips marks AND collapses to plain
             paragraph (matches Outlook's "Clear all formatting"). -->
        <button class="rt-btn" title="Clear all formatting" onclick={clearFormatting}>
          <span class="rt-btn-icon">🧹</span>
          <span class="rt-btn-label">Clear</span>
        </button>
      {:else if activeTab === 'insert'}
        <button class="rt-btn {active('link')}" title="Insert link" onclick={setLink}>
          <span class="rt-btn-icon">🔗</span>
          <span class="rt-btn-label">Link</span>
        </button>
        <button class="rt-btn" title="Insert image from local file" onclick={() => addImageFromFile()}>
          <span class="rt-btn-icon">🖼️</span>
          <span class="rt-btn-label">Image</span>
        </button>
        <button
          class="rt-btn"
          title={onrequestncimage ? 'Insert image from Nextcloud' : 'Insert image from URL'}
          onclick={() => addImageFromNcOrUrl()}
        >
          <span class="rt-btn-icon">{onrequestncimage ? '☁️' : '🌐'}</span>
          <span class="rt-btn-label">{onrequestncimage ? 'NC image' : 'From URL'}</span>
        </button>

        <span class="rt-divider"></span>

        <!-- Table picker -->
        <div class="relative inline-block">
          <button class="rt-btn" title="Insert table at cursor" onclick={openTablePicker}>
            <span class="rt-btn-icon">▦</span>
            <span class="rt-btn-label">Table</span>
          </button>
          {#if showTablePicker}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="absolute left-0 top-full mt-1 z-50 p-2 bg-white dark:bg-surface-800 border border-surface-300 dark:border-surface-600 rounded-md shadow-lg"
              onmouseleave={() => { tableHoverRows = 0; tableHoverCols = 0 }}
            >
              <div class="text-xs text-surface-500 mb-1 text-center">
                {tableHoverRows > 0 ? `${tableHoverRows} × ${tableHoverCols}` : 'Pick size'}
              </div>
              <div class="grid gap-0.5" style="grid-template-columns: repeat({TABLE_GRID}, 1fr)">
                {#each { length: TABLE_GRID } as _, r}
                  {#each { length: TABLE_GRID } as _, c}
                    <button
                      type="button"
                      aria-label="{r + 1} × {c + 1} table"
                      class="w-4 h-4 border rounded-sm cursor-pointer transition-colors
                        {r < tableHoverRows && c < tableHoverCols
                          ? 'bg-primary-500/40 border-primary-500'
                          : 'bg-surface-100 dark:bg-surface-700 border-surface-300 dark:border-surface-600'}"
                      onmouseenter={() => { tableHoverRows = r + 1; tableHoverCols = c + 1 }}
                      onclick={() => insertTable(r + 1, c + 1)}
                      tabindex="-1"
                    ></button>
                  {/each}
                {/each}
              </div>
            </div>
          {/if}
        </div>

        <button class="rt-btn" title="Horizontal rule" onclick={() => cmd().setHorizontalRule().run()}>
          <span class="rt-btn-icon">―</span>
          <span class="rt-btn-label">HR</span>
        </button>

        <span class="rt-divider"></span>

        <!-- Emoji picker — popup grid of curated emojis (#103
             follow-up).  Click outside or pick an emoji to dismiss. -->
        <div class="relative inline-block">
          <button class="rt-btn" title="Insert emoji" onclick={() => (showEmojiPicker = !showEmojiPicker)}>
            <span class="rt-btn-icon">😀</span>
            <span class="rt-btn-label">Emoji</span>
          </button>
          {#if showEmojiPicker}
            <div
              class="absolute left-0 top-full mt-1 z-50"
              role="menu"
              tabindex="-1"
              onclick={(e) => e.stopPropagation()}
              onkeydown={(e) => e.key === 'Escape' && (showEmojiPicker = false)}
            >
              <EmojiPicker allowClear={false} onpick={(e) => insertEmoji(e)} />
            </div>
          {/if}
        </div>

        <span class="rt-divider"></span>

        <!-- Table editing tools — visible always, but disabled when
             the cursor isn't inside a table.  Sits in the Insert
             tab next to the table-creation picker so the user
             reaches for one ribbon section regardless of whether
             they're creating or editing.  Background-colour input
             writes to a custom `backgroundColor` attribute on the
             cell (TableCell extension above) which renders as
             inline `style="background-color: …"` for cross-client
             email compatibility. -->
        {@const tblOn = tableActive()}
        <button class="rt-btn" title="Add row above" disabled={!tblOn} onclick={tblAddRowAbove}>
          <span class="rt-btn-icon">⤴︎</span>
          <span class="rt-btn-label">Row above</span>
        </button>
        <button class="rt-btn" title="Add row below" disabled={!tblOn} onclick={tblAddRowBelow}>
          <span class="rt-btn-icon">⤵︎</span>
          <span class="rt-btn-label">Row below</span>
        </button>
        <button class="rt-btn" title="Add column left" disabled={!tblOn} onclick={tblAddColLeft}>
          <span class="rt-btn-icon">⇤</span>
          <span class="rt-btn-label">Col left</span>
        </button>
        <button class="rt-btn" title="Add column right" disabled={!tblOn} onclick={tblAddColRight}>
          <span class="rt-btn-icon">⇥</span>
          <span class="rt-btn-label">Col right</span>
        </button>
        <button class="rt-btn" title="Delete current row" disabled={!tblOn} onclick={tblDeleteRow}>
          <span class="rt-btn-icon">−↔</span>
          <span class="rt-btn-label">Del row</span>
        </button>
        <button class="rt-btn" title="Delete current column" disabled={!tblOn} onclick={tblDeleteCol}>
          <span class="rt-btn-icon">−↕</span>
          <span class="rt-btn-label">Del col</span>
        </button>
        <label class="rt-btn cursor-pointer" title="Cell background colour" class:opacity-50={!tblOn}>
          <span class="rt-btn-icon">🎨</span>
          <span class="rt-btn-label">Cell colour</span>
          <input type="color" class="w-0 h-0 opacity-0 absolute" disabled={!tblOn} onchange={tblSetCellColor} />
        </label>
        <button class="rt-btn" title="Clear cell background colour" disabled={!tblOn} onclick={tblClearCellColor}>
          <span class="rt-btn-icon">⌫</span>
          <span class="rt-btn-label">Clear fill</span>
        </button>
        <button class="rt-btn" title="Delete entire table" disabled={!tblOn} onclick={tblDelete}>
          <span class="rt-btn-icon">🗑</span>
          <span class="rt-btn-label">Del table</span>
        </button>
      {:else if activeTab === 'layout'}
        <!-- Headings -->
        <button class="rt-btn {active('heading', { level: 1 })}" title="Heading 1" onclick={() => toggleHeading(1)}>
          <span class="rt-btn-icon">H₁</span>
          <span class="rt-btn-label">Heading 1</span>
        </button>
        <button class="rt-btn {active('heading', { level: 2 })}" title="Heading 2" onclick={() => toggleHeading(2)}>
          <span class="rt-btn-icon">H₂</span>
          <span class="rt-btn-label">Heading 2</span>
        </button>
        <button class="rt-btn {active('heading', { level: 3 })}" title="Heading 3" onclick={() => toggleHeading(3)}>
          <span class="rt-btn-icon">H₃</span>
          <span class="rt-btn-label">Heading 3</span>
        </button>

        <span class="rt-divider"></span>

        <!-- Lists -->
        <button class="rt-btn {active('bulletList')}" title="Bullet list" onclick={() => $editor?.chain().focus().toggleBulletList().run()}>
          <span class="rt-btn-icon">•</span>
          <span class="rt-btn-label">Bullets</span>
        </button>
        <button class="rt-btn {active('orderedList')}" title="Numbered list" onclick={() => $editor?.chain().focus().toggleOrderedList().run()}>
          <span class="rt-btn-icon">1.</span>
          <span class="rt-btn-label">Numbered</span>
        </button>

        <span class="rt-divider"></span>

        <!-- Alignment -->
        <button class="rt-btn {activeAttrs({ textAlign: 'left' })}" title="Align left" onclick={() => $editor?.chain().focus().setTextAlign('left').run()}>
          <span class="rt-btn-icon">⇤</span>
          <span class="rt-btn-label">Left</span>
        </button>
        <button class="rt-btn {activeAttrs({ textAlign: 'center' })}" title="Align center" onclick={() => $editor?.chain().focus().setTextAlign('center').run()}>
          <span class="rt-btn-icon">≡</span>
          <span class="rt-btn-label">Center</span>
        </button>
        <button class="rt-btn {activeAttrs({ textAlign: 'right' })}" title="Align right" onclick={() => $editor?.chain().focus().setTextAlign('right').run()}>
          <span class="rt-btn-icon">⇥</span>
          <span class="rt-btn-label">Right</span>
        </button>
        <button class="rt-btn {activeAttrs({ textAlign: 'justify' })}" title="Justify" onclick={() => $editor?.chain().focus().setTextAlign('justify').run()}>
          <span class="rt-btn-icon">☰</span>
          <span class="rt-btn-label">Justify</span>
        </button>

        <span class="rt-divider"></span>

        <button class="rt-btn {active('blockquote')}" title="Blockquote" onclick={() => cmd().toggleBlockquote().run()}>
          <span class="rt-btn-icon">❝</span>
          <span class="rt-btn-label">Quote</span>
        </button>
      {:else}
        {#each extraTabs as t (t.id)}
          {#if activeTab === t.id}
            {@render t.content()}
          {/if}
        {/each}
      {/if}
    </div>
  </div>

  <!-- Editor area. `flex-1 min-h-0` lets it shrink/grow with the
       wrapper's available height; `overflow-y-auto` scrolls internally
       once the content exceeds what fits. When the Compose modal is
       resized taller, this is what absorbs the new space. -->
  <div class="flex-1 min-h-0 border border-surface-200 dark:border-surface-700 rounded-b-md bg-surface-50 dark:bg-surface-950 overflow-y-auto">
    <EditorContent editor={$editor} />
  </div>
</div>

<!-- ── `@` contact picker popup ────────────────────────────
     `position: fixed` anchored to the trigger char's bounding rect
     (computed in `anchorBelow`). z-60 so we sit on top of the
     Compose modal's z-50 backdrop. The whole panel only renders
     while the suggestion plugin says it should be visible — the
     popup never lingers in the DOM tree when no `@` is active. -->
{#if mentionPicker.visible}
  <ul
    class="fixed z-60 max-h-72 min-w-72 overflow-y-auto rounded-md border border-surface-300
           dark:border-surface-700 bg-surface-50 dark:bg-surface-900 shadow-lg py-1 text-sm"
    style="left: {mentionPicker.position.left}px; top: {mentionPicker.position.top}px;"
    role="listbox"
  >
    {#if mentionPicker.items.length === 0}
      <li class="px-3 py-2 text-xs text-surface-500">No matching contacts</li>
    {:else}
      {#each mentionPicker.items as c, i (c.id)}
        <li
          role="option"
          aria-selected={i === mentionPicker.selectedIndex}
          class="flex items-center gap-3 px-3 py-1.5 cursor-pointer
                 {i === mentionPicker.selectedIndex
                   ? 'bg-primary-500/15'
                   : 'hover:bg-surface-200 dark:hover:bg-surface-800'}"
          onmousedown={(e) => {
            // mousedown so we commit before the editor sees a
            // focus-loss and tears the popup down.
            e.preventDefault()
            mentionPicker.command?.(c)
          }}
        >
          {#if c.photoUrl}
            <img src={c.photoUrl} alt="" loading="lazy"
                 class="w-7 h-7 rounded-full object-cover shrink-0" />
          {:else}
            <div class="w-7 h-7 rounded-full bg-surface-300 dark:bg-surface-700
                        flex items-center justify-center text-[10px] font-semibold shrink-0">
              {c.label.trim().charAt(0).toUpperCase() || '?'}
            </div>
          {/if}
          <div class="flex-1 min-w-0">
            <p class="font-medium truncate">{c.label}</p>
            <p class="text-xs text-surface-500 truncate">
              {c.email}{#if c.hint} · {c.hint}{/if}
            </p>
          </div>
        </li>
        <!-- Divider after the last `participants` row, but only when
             there are also `others` below it. Pure visual separator
             — not selectable, not in the keyboard cycle. -->
        {#if i === mentionPicker.participantsCount - 1
              && i < mentionPicker.items.length - 1}
          <li
            aria-hidden="true"
            class="my-1 mx-2 border-t border-surface-200 dark:border-surface-700"
          ></li>
        {/if}
      {/each}
    {/if}
  </ul>
{/if}

<!-- ── `/` attachment picker popup ─────────────────────────
     Same shape as the contact picker, narrower because rows are
     just a filename + a paperclip glyph. Only attachments with a
     `content_id` show up — everything else has nothing to link to. -->
{#if attachmentPicker.visible}
  <ul
    class="fixed z-60 max-h-72 min-w-64 overflow-y-auto rounded-md border border-surface-300
           dark:border-surface-700 bg-surface-50 dark:bg-surface-900 shadow-lg py-1 text-sm"
    style="left: {attachmentPicker.position.left}px; top: {attachmentPicker.position.top}px;"
    role="listbox"
  >
    {#if attachmentPicker.items.length === 0}
      <li class="px-3 py-2 text-xs text-surface-500">No attachments to reference</li>
    {:else}
      {#each attachmentPicker.items as a, i (a.content_id)}
        <li
          role="option"
          aria-selected={i === attachmentPicker.selectedIndex}
          class="flex items-center gap-2 px-3 py-1.5 cursor-pointer
                 {i === attachmentPicker.selectedIndex
                   ? 'bg-primary-500/15'
                   : 'hover:bg-surface-200 dark:hover:bg-surface-800'}"
          onmousedown={(e) => {
            e.preventDefault()
            attachmentPicker.command?.(a)
          }}
        >
          <span class="text-base shrink-0">📎</span>
          <span class="truncate">{a.filename}</span>
        </li>
      {/each}
    {/if}
  </ul>
{/if}
{/if}
