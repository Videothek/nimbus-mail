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
  }
  let {
    content = '',
    placeholder = 'Write your message\u2026',
    onchange,
    onready,
    onrequestncimage,
  }: Props = $props()

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
      TableCell,
      TableHeader,
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
  const FONT_FAMILIES: Array<{ label: string; css: string }> = [
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
  let showFontPicker = $state(false)

  /** Label for the toolbar button, reflecting the font at the current
      cursor position. `$editor` is a svelte-tiptap store that re-emits
      on every editor transaction, so this function re-runs after every
      selection change or edit — the label flips in step with the
      cursor. Falls back to the generic "Font" when the cursor sits in
      text carrying a family we don't have a pretty label for. */
  function currentFontLabel(): string {
    if (!$editor) return 'Font'
    const css = ($editor.getAttributes('textStyle')?.fontFamily as string | undefined) ?? ''
    const match = FONT_FAMILIES.find((f) => f.css === css)
    return match?.label ?? 'Font'
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

  function insertTable(rows: number, cols: number) {
    cmd().insertTable({ rows, cols, withHeaderRow: true }).run()
    showTablePicker = false
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
  const ACTIVE_CLS = 'bg-surface-300 dark:bg-surface-600'

  function active(name: string, attrs?: Record<string, unknown>): string {
    return $editor?.isActive(name, attrs) ? ACTIVE_CLS : ''
  }

  function activeAttrs(attrs: Record<string, unknown>): string {
    return $editor?.isActive(attrs) ? ACTIVE_CLS : ''
  }
</script>

<style>
  /* Toolbar buttons — small, consistent touch targets. */
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
  <!-- Toolbar -->
  <div class="flex flex-wrap items-center gap-0.5 px-2 py-1.5 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800 text-sm">
    <!-- Font family picker — dropdown because 6 named families
         wouldn't fit as individual toolbar buttons. The trigger
         label tracks the font at the cursor: `currentFontLabel`
         reads Tiptap's active textStyle attrs via `$editor`, which
         re-emits on every transaction, so the button reflects
         moves through text of different faces in real time.
         Clicking outside closes the menu (see global listener
         inside the `$effect` below). -->
    <div class="relative inline-block">
      <button
        type="button"
        class="tb"
        title="Font family"
        onclick={() => (showFontPicker = !showFontPicker)}
      >
        {currentFontLabel()} ▾
      </button>
      {#if showFontPicker}
        <div
          class="absolute z-20 mt-1 min-w-40 rounded-md border border-surface-200 dark:border-surface-700 bg-surface-50 dark:bg-surface-900 shadow-md py-1"
          role="menu"
          tabindex="-1"
          onclick={(e) => e.stopPropagation()}
          onkeydown={(e) => e.key === 'Escape' && (showFontPicker = false)}
        >
          {#each FONT_FAMILIES as f (f.label)}
            <button
              type="button"
              class="w-full text-left px-3 py-1 text-sm hover:bg-surface-200 dark:hover:bg-surface-800"
              style={f.css ? `font-family: ${f.css};` : ''}
              onclick={() => setFont(f.css)}
            >{f.label}</button>
          {/each}
        </div>
      {/if}
    </div>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Text style group -->
    <button class="tb {active('bold')}" title="Bold" onclick={() => $editor?.chain().focus().toggleBold().run()}>
      <strong>B</strong>
    </button>
    <button class="tb {active('italic')}" title="Italic" onclick={() => $editor?.chain().focus().toggleItalic().run()}>
      <em>I</em>
    </button>
    <button class="tb {active('underline')}" title="Underline" onclick={() => $editor?.chain().focus().toggleUnderline().run()}>
      <u>U</u>
    </button>
    <button class="tb {active('strike')}" title="Strikethrough" onclick={() => $editor?.chain().focus().toggleStrike().run()}>
      <s>S</s>
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Headings -->
    <button class="tb {active('heading', { level: 1 })}" title="Heading 1" onclick={() => toggleHeading(1)}>
      H1
    </button>
    <button class="tb {active('heading', { level: 2 })}" title="Heading 2" onclick={() => toggleHeading(2)}>
      H2
    </button>
    <button class="tb {active('heading', { level: 3 })}" title="Heading 3" onclick={() => toggleHeading(3)}>
      H3
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Lists -->
    <button class="tb {active('bulletList')}" title="Bullet list" onclick={() => $editor?.chain().focus().toggleBulletList().run()}>
      &#8226; List
    </button>
    <button class="tb {active('orderedList')}" title="Numbered list" onclick={() => $editor?.chain().focus().toggleOrderedList().run()}>
      1. List
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Alignment -->
    <button class="tb {activeAttrs({ textAlign: 'left' })}" title="Align left" onclick={() => $editor?.chain().focus().setTextAlign('left').run()}>
      &#x2261;L
    </button>
    <button class="tb {activeAttrs({ textAlign: 'center' })}" title="Align center" onclick={() => $editor?.chain().focus().setTextAlign('center').run()}>
      &#x2261;C
    </button>
    <button class="tb {activeAttrs({ textAlign: 'right' })}" title="Align right" onclick={() => $editor?.chain().focus().setTextAlign('right').run()}>
      &#x2261;R
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Colors -->
    <label class="tb cursor-pointer" title="Text color">
      A
      <input type="color" class="w-0 h-0 opacity-0 absolute" onchange={setColor} />
    </label>
    <label class="tb cursor-pointer" title="Highlight color">
      <span class="bg-yellow-200 px-0.5 rounded-sm">H</span>
      <input type="color" value="#fde68a" class="w-0 h-0 opacity-0 absolute" onchange={setHighlight} />
    </label>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Insert group -->
    <button class="tb {active('link')}" title="Insert link" onclick={setLink}>
      Link
    </button>

    <!-- Image: two entry points. "Image" picks a local file and
         embeds it as a data URL. "NC" opens the parent's Nextcloud
         file picker (via `onrequestncimage`) so the user can drop
         in a file they already have on their Nextcloud without
         saving it locally first — consistent with how attachments
         work in the Compose toolbar. Falls back to a URL prompt if
         the embedder didn't wire up the Nextcloud callback. -->
    <div class="relative inline-block">
      <button class="tb" title="Insert image from local file" onclick={() => addImageFromFile()}>
        Image
      </button>
      <button
        class="tb text-[10px]"
        title={onrequestncimage ? 'Insert image from Nextcloud' : 'Insert image from URL'}
        onclick={() => addImageFromNcOrUrl()}
      >
        {onrequestncimage ? 'NC' : 'URL'}
      </button>
    </div>

    <!-- Table: Outlook-style grid picker -->
    <div class="relative inline-block">
      <button class="tb" title="Insert table" onclick={() => (showTablePicker = !showTablePicker)}>
        Table
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
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div
                  class="w-4 h-4 border rounded-sm cursor-pointer transition-colors
                    {r < tableHoverRows && c < tableHoverCols
                      ? 'bg-primary-500/40 border-primary-500'
                      : 'bg-surface-100 dark:bg-surface-700 border-surface-300 dark:border-surface-600'}"
                  onmouseenter={() => { tableHoverRows = r + 1; tableHoverCols = c + 1 }}
                  onclick={() => insertTable(r + 1, c + 1)}
                  role="button"
                  tabindex="-1"
                ></div>
              {/each}
            {/each}
          </div>
        </div>
      {/if}
    </div>

    <button class="tb" title="Horizontal rule" onclick={() => cmd().setHorizontalRule().run()}>
      &#x2015;
    </button>
    <button class="tb {active('blockquote')}" title="Blockquote" onclick={() => cmd().toggleBlockquote().run()}>
      &#x201C;
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Undo / Redo -->
    <button class="tb" title="Undo (Ctrl+Z)" onclick={() => doUndo()}>
      &#x21A9;
    </button>
    <button class="tb" title="Redo (Ctrl+Y)" onclick={() => doRedo()}>
      &#x21AA;
    </button>
  </div>

  <!-- Editor area. `flex-1 min-h-0` lets it shrink/grow with the
       wrapper's available height; `overflow-y-auto` scrolls internally
       once the content exceeds what fits. When the Compose modal is
       resized taller, this is what absorbs the new space. -->
  <div class="flex-1 min-h-0 border border-surface-200 dark:border-surface-700 rounded-b-md bg-surface-50 dark:bg-surface-950 overflow-y-auto">
    <EditorContent editor={$editor} />
  </div>
</div>
{/if}
